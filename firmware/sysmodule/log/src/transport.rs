//! sysmodule_log as a `HostTransport`.
//!
//! Handles incoming bytes on USART2 as a second tunneled-IPC transport
//! alongside `sysmodule_usb_protocol_host`. The USART wire format is a
//! stream of COBS chunks (same framing as the log output), each
//! prefixed with a type byte from `rcard_log::wire`:
//!
//!   0x01 log fragment      (device→host only — ignored inbound)
//!   0x02 ipc reply         (device→host only — ignored inbound)
//!   0x03 ipc request       (host→device — staged for host_proxy)
//!
//! Reassembly state (COBS accumulator, pending frame) lives here.

use generated::peers::PEERS;
use once_cell::GlobalState;
use sysmodule_host_transport_api::*;

use crate::{send_ipc_reply, Reactor, Usart, USART};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Max wire-format `IpcRequest` frame we can stage. Matches
/// `usb_protocol_host`'s `MAX_FRAME` so both transports accept the same
/// host-side request ceiling.
const MAX_FRAME: usize = 8704;

/// Max COBS-encoded chunk we accumulate for one inbound frame. Budget:
/// 1 (type) + MAX_FRAME body + ~1 byte per 254 of COBS overhead + 1 delimiter.
/// Round up generously.
const MAX_COBS_CHUNK: usize = MAX_FRAME + 64;

/// True iff this firmware build includes a `sysmodule_host_proxy` task.
/// Resolved at codegen time via `peers`. When false, inbound IPC requests
/// get a `NoHostForwarding` reply instead of being staged.
const HOST_FORWARDING_AVAILABLE: bool = PEERS.sysmodule_host_proxy.is_some();

// ---------------------------------------------------------------------------
// Pending request staging
// ---------------------------------------------------------------------------

pub struct PendingRequest {
    pub buf: [u8; MAX_FRAME],
    pub len: usize,
    /// True between `stage()` and `deliver_reply()`'s clear.
    pub set: bool,
}

impl PendingRequest {
    const fn new() -> Self {
        Self {
            buf: [0u8; MAX_FRAME],
            len: 0,
            set: false,
        }
    }
}

pub static PENDING: GlobalState<PendingRequest> = GlobalState::new(PendingRequest::new());

// ---------------------------------------------------------------------------
// Inbound COBS accumulator + frame dispatcher
// ---------------------------------------------------------------------------

struct CobsAccum {
    buf: [u8; MAX_COBS_CHUNK],
    len: usize,
    overflowed: bool,
}

impl CobsAccum {
    const fn new() -> Self {
        Self {
            buf: [0u8; MAX_COBS_CHUNK],
            len: 0,
            overflowed: false,
        }
    }

    fn reset(&mut self) {
        self.len = 0;
        self.overflowed = false;
    }

    fn push(&mut self, b: u8) {
        if self.len >= self.buf.len() {
            self.overflowed = true;
            return;
        }
        self.buf[self.len] = b;
        self.len += 1;
    }
}

static COBS_ACCUM: GlobalState<CobsAccum> = GlobalState::new(CobsAccum::new());

/// Scratch buffer for COBS-decoding one complete chunk. Parked in BSS
/// rather than on the stack because `feed_byte` runs inside the
/// `usart_event` notification handler and 8 KB on the stack per wakeup
/// would dominate the task's stack budget.
static COBS_DECODE: GlobalState<[u8; MAX_COBS_CHUNK]> =
    GlobalState::new([0u8; MAX_COBS_CHUNK]);

/// Staging buffer for `deliver_reply` to copy the caller's lease into
/// before handing it to `send_ipc_reply`. Parked in BSS for the same
/// reason as the other big log buffers — an 8.7 KB stack frame on a
/// task with an 8 KiB stack is a guaranteed overflow.
static DELIVER_BODY: GlobalState<[u8; MAX_FRAME]> =
    GlobalState::new([0u8; MAX_FRAME]);

/// Drain the USART2 RX ring and feed bytes through the COBS accumulator.
/// On each complete chunk (terminated by 0x00), dispatch by type byte.
///
/// Called from the `usart_event` notification handler in main.
pub fn handle_usart_rx() {
    let Some(usart) = USART.get() else { return };

    let mut chunk = [0u8; 64];
    loop {
        let n = match usart.read(&mut chunk) {
            Ok(n) => n as usize,
            Err(_) => return,
        };
        if n == 0 {
            break;
        }
        for &b in &chunk[..n] {
            feed_byte(usart, b);
        }
    }
}

fn feed_byte(usart: &Usart, b: u8) {
    if b != 0x00 {
        COBS_ACCUM.with(|acc| acc.push(b)).unwrap();
        return;
    }

    // End of COBS chunk. Decode into the static scratch buffer and
    // dispatch. We have to release the COBS_ACCUM borrow before
    // grabbing COBS_DECODE so we don't hold two statics at once; the
    // first `with` copies the accumulated bytes out.
    let decoded_len = COBS_ACCUM
        .with(|acc| {
            if acc.len == 0 || acc.overflowed {
                // Oversized / empty COBS chunk — drop silently.
                // sysmodule_log can't log through itself, so parse
                // errors stay invisible.
                acc.reset();
                return None;
            }
            let res = COBS_DECODE.with(|decoded| {
                cobs::decode(&acc.buf[..acc.len], decoded).ok()
            });
            acc.reset();
            res.flatten()
        })
        .flatten();

    if let Some(n) = decoded_len {
        COBS_DECODE.with(|decoded| dispatch_chunk(usart, &decoded[..n]));
    }
}

fn dispatch_chunk(usart: &Usart, chunk: &[u8]) {
    if chunk.is_empty() {
        return;
    }
    match chunk[0] {
        rcard_log::wire::TYPE_IPC_REQUEST => stage_ipc_request(usart, &chunk[1..]),
        rcard_log::wire::TYPE_CONTROL_REQUEST => handle_control_request(usart, &chunk[1..]),
        // Log fragments and IPC replies are device→host only; silently
        // drop them if the host echoes them back. Unknown types too.
        _ => {}
    }
}

/// Handle a non-tunneled host → device control simple frame.
///
/// `body` is a complete `rcard_usb_proto` wire frame (header + 1-byte
/// opcode + opcode-specific payload). Only `MoshiMoshi` is recognized
/// today; it's answered in-band with an `Awake` simple frame (same
/// payload as the boot-time announcement) so the host can re-discover
/// device identity without a power cycle and without `host_proxy`.
fn handle_control_request(_usart: &Usart, body: &[u8]) {
    use rcard_usb_proto::frame::FrameType;
    use rcard_usb_proto::header::{FrameHeader, HEADER_SIZE};
    use rcard_usb_proto::messages::OP_MOSHI_MOSHI;

    let Ok(header) = FrameHeader::decode(body) else {
        return;
    };
    if header.frame_type != FrameType::Simple {
        return;
    }
    let Some(&opcode) = body.get(HEADER_SIZE) else {
        return;
    };
    if opcode != OP_MOSHI_MOSHI {
        return;
    }

    // Echo the request's seq on the Awake reply so the host can pair them.
    crate::send_awake(header.seq);
}

fn stage_ipc_request(usart: &Usart, body: &[u8]) {
    // `body` is a full rcard_usb_proto wire frame (header + IpcRequest body).
    // We stage it verbatim and wake host_proxy.

    // Peel the 5-byte FrameHeader directly out of `body` — don't use
    // `FrameReader<MAX_FRAME>`, whose 8 KB internal buffer is a stack
    // array that gets inlined into this task's notification closure
    // and blows our 8 KiB stack budget.
    use rcard_usb_proto::frame::FrameType;
    use rcard_usb_proto::header::FrameHeader;

    let Ok(header) = FrameHeader::decode(body) else {
        return;
    };
    if header.frame_type != FrameType::IpcRequest {
        return;
    }
    let seq = header.seq;

    if !HOST_FORWARDING_AVAILABLE {
        // No host_proxy in this build — reply NACK immediately.
        send_tunnel_error(
            usart,
            seq,
            rcard_usb_proto::messages::TunnelErrorCode::NoHostForwarding,
        );
        return;
    }

    // Attempt to stage. `StageOutcome` tells us *why* we failed so we
    // can send an informative NACK — silently dropping a request leaves
    // a retrying host guessing.
    enum StageOutcome {
        Staged,
        Busy,
        TooLarge,
    }

    let outcome = PENDING
        .with(|p| {
            if p.set {
                return StageOutcome::Busy;
            }
            if body.len() > p.buf.len() {
                return StageOutcome::TooLarge;
            }
            p.buf[..body.len()].copy_from_slice(body);
            p.len = body.len();
            p.set = true;
            StageOutcome::Staged
        })
        .unwrap_or(StageOutcome::Busy);

    match outcome {
        StageOutcome::Staged => {
            // Wake host_proxy. Coalesce so a burst of USART frames (which
            // shouldn't happen under the synchronous protocol, but is cheap
            // to guard against) collapses to one notification.
            let _ = Reactor::push(
                generated::notifications::GROUP_ID_HOST_REQUEST,
                0,
                20,
                sysmodule_reactor_api::OverflowStrategy::Reject,
            );
        }
        StageOutcome::Busy => {
            send_tunnel_error(
                usart,
                seq,
                rcard_usb_proto::messages::TunnelErrorCode::Busy,
            );
        }
        StageOutcome::TooLarge => {
            send_tunnel_error(
                usart,
                seq,
                rcard_usb_proto::messages::TunnelErrorCode::BadRequest,
            );
        }
    }
}

fn send_tunnel_error(
    usart: &Usart,
    seq: u16,
    code: rcard_usb_proto::messages::TunnelErrorCode,
) {
    let msg = rcard_usb_proto::messages::TunnelError { code };
    let mut body = [0u8; 16];
    if let Some(n) = rcard_usb_proto::simple::encode_simple(&msg, &mut body, seq) {
        let _ = send_ipc_reply(usart, &body[..n]);
    }
}

// ---------------------------------------------------------------------------
// HostTransport server impl
// ---------------------------------------------------------------------------

pub struct LogHostTransport;

impl HostTransport for LogHostTransport {
    fn fetch_pending_request(
        _meta: ipc::Meta,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<u32, HostTransportError> {
        PENDING
            .with(|p| {
                if !p.set {
                    return Err(HostTransportError::NoPendingRequest);
                }
                if buf.len() < p.len {
                    return Err(HostTransportError::LeaseTooSmall);
                }
                for (i, &b) in p.buf[..p.len].iter().enumerate() {
                    let _ = buf.write(i, b);
                }
                Ok(p.len as u32)
            })
            .unwrap_or(Err(HostTransportError::NoPendingRequest))
    }

    fn deliver_reply(
        _meta: ipc::Meta,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<(), HostTransportError> {
        let len = buf.len();
        if len > MAX_FRAME {
            return Err(HostTransportError::LeaseTooSmall);
        }

        let Some(usart) = USART.get() else {
            return Err(HostTransportError::WireWriteFailed);
        };

        // Stage the lease into DELIVER_BODY (BSS), then hand it to
        // send_ipc_reply, which borrows its own separate statics for
        // raw + encoded staging.
        let result = DELIVER_BODY
            .with(|body| {
                let _ = buf.read_range(0, &mut body[..len]);
                send_ipc_reply(usart, &body[..len])
            })
            .unwrap_or(Err(()));

        // Always clear the pending slot; dispatch is complete from
        // host_proxy's perspective.
        PENDING
            .with(|p| {
                p.set = false;
                p.len = 0;
            })
            .unwrap();

        result.map_err(|_| HostTransportError::WireWriteFailed)
    }
}
