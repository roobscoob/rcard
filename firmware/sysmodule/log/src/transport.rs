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
//! Memory budget: one shared `FRAME_BUF` (8.7 KB) serves both the
//! receive path (streaming COBS decode via `DecoderState`) and the
//! send path (COBS encode output). No separate accumulator, decode
//! scratch, or staging buffers.

use generated::peers::PEERS;
use once_cell::GlobalState;
use sysmodule_host_transport_api::*;

use crate::{send_ipc_reply, Reactor, Usart, USART};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Max wire-format `IpcRequest` frame we can stage. Derived from the
/// protocol constants so all transports accept the same ceiling.
const MAX_FRAME: usize = rcard_usb_proto::MAX_DECODED_FRAME;

/// Max COBS-encoded chunk. Budget: 1 (type) + MAX_FRAME body + ~34 bytes
/// of COBS overhead (one per 254) + 1 delimiter. Rounded up.
const MAX_COBS_CHUNK: usize = MAX_FRAME + 64;

/// True iff this firmware build includes a `sysmodule_host_proxy` task.
const HOST_FORWARDING_AVAILABLE: bool = PEERS.sysmodule_host_proxy.is_some();

// ---------------------------------------------------------------------------
// Shared frame buffer
// ---------------------------------------------------------------------------

/// Single buffer for both receive (streaming COBS decode output) and
/// send (COBS encode output for deliver_reply). The protocol is
/// synchronous — only one direction is active at a time.
pub struct FrameBuffer {
    pub buf: [u8; MAX_COBS_CHUNK],
    /// Write position for the streaming decoder.
    write_pos: usize,
    /// Length of the decoded/staged frame.
    pub staged_len: usize,
    /// True between successful staging and `deliver_reply`'s clear.
    pub staged: bool,
    /// Set when `push_decoded_byte` drops bytes because the decoded
    /// output exceeds `MAX_COBS_CHUNK`. Checked (and cleared) on
    /// `DataComplete` to reject truncated frames.
    truncated: bool,
}

impl FrameBuffer {
    const fn new() -> Self {
        Self {
            buf: [0u8; MAX_COBS_CHUNK],
            write_pos: 0,
            staged_len: 0,
            staged: false,
            truncated: false,
        }
    }

    fn push_decoded_byte(&mut self, b: u8) {
        if self.write_pos < self.buf.len() {
            self.buf[self.write_pos] = b;
            self.write_pos += 1;
        } else {
            self.truncated = true;
        }
    }

    fn discard(&mut self) {
        self.write_pos = 0;
        self.truncated = false;
    }
}

pub static FRAME_BUF: GlobalState<FrameBuffer> = GlobalState::new(FrameBuffer::new());

/// Streaming COBS decoder state.
static DECODER: GlobalState<cobs::DecoderState> = GlobalState::new(cobs::DecoderState::Idle);

// ---------------------------------------------------------------------------
// Inbound byte processing
// ---------------------------------------------------------------------------

/// Drain the USART2 RX ring and feed bytes through the streaming COBS
/// decoder. On each complete frame, dispatch by type byte.
pub fn handle_usart_rx() {
    let Some(usart) = USART.get() else { return };

    let busy = FRAME_BUF.with(|fb| fb.staged).unwrap_or(false);
    if busy {
        return;
    }

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
    use cobs::DecodeResult;

    let result = DECODER.with(|dec| dec.feed(b));

    match result {
        Some(Ok(DecodeResult::DataContinue(byte))) => {
            FRAME_BUF.with(|fb| fb.push_decoded_byte(byte));
        }
        Some(Ok(DecodeResult::DataComplete)) => {
            FRAME_BUF.with(|fb| {
                let decoded_len = fb.write_pos;
                let was_truncated = fb.truncated;
                fb.write_pos = 0;
                fb.truncated = false;

                if decoded_len == 0 || was_truncated {
                    return;
                }

                let type_byte = fb.buf[0];
                // Shift the body to the front so FRAME_BUF holds just
                // the body (without the type prefix). This way
                // fetch_pending_request can read fb.buf[..staged_len]
                // and get the raw IPC frame without the wire type byte.
                fb.buf.copy_within(1..decoded_len, 0);
                let body_len = decoded_len - 1;

                match type_byte {
                    rcard_log::wire::TYPE_IPC_REQUEST => {
                        if stage_ipc_request(usart, &fb.buf[..body_len]) {
                            fb.staged_len = body_len;
                            fb.staged = true;
                        }
                    }
                    rcard_log::wire::TYPE_CONTROL_REQUEST => {
                        handle_control_request(usart, &fb.buf[..body_len]);
                    }
                    _ => {}
                }
            });
        }
        Some(Ok(DecodeResult::NoData)) => {
            // Header byte or idle sentinel — no output yet.
        }
        Some(Err(_)) | None => {
            // Decode error or GlobalState contention — discard.
            FRAME_BUF.with(|fb| fb.discard());
        }
    }
}

/// Handle a non-tunneled host → device control simple frame.
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

    crate::send_awake(header.seq);

    // Mirror the identity payload onto USART1 by IPC'ing the supervisor.
    // Lets the host identify a USART1 port that's connected to this device
    // even though USART1 has no RX path of its own. Best-effort.
    let uid = crate::CACHED_UID.get().copied().unwrap_or([0u8; 16]);
    let firmware_id = ::generated::build_info::BUILD_ID_BYTES;
    crate::emit_supervisor_hello(uid, firmware_id);
}

/// Validate and stage an IPC request frame for host_proxy. Returns
/// `true` if the frame was successfully staged and host_proxy was
/// notified — the caller must set `staged = true` only in that case.
fn stage_ipc_request(usart: &Usart, body: &[u8]) -> bool {
    use rcard_usb_proto::frame::FrameType;
    use rcard_usb_proto::header::FrameHeader;

    let Ok(header) = FrameHeader::decode(body) else {
        return false;
    };
    if header.frame_type != FrameType::IpcRequest {
        return false;
    }
    let seq = header.seq;

    if !HOST_FORWARDING_AVAILABLE {
        send_tunnel_error(
            usart,
            seq,
            rcard_usb_proto::messages::TunnelErrorCode::NoHostForwarding,
        );
        return false;
    }

    // Data is already in FRAME_BUF (placed by the streaming decoder).
    // Wake host_proxy; the caller sets `staged = true`.
    let _ = Reactor::push(
        generated::notifications::GROUP_ID_HOST_REQUEST,
        0,
        20,
        sysmodule_reactor_api::OverflowStrategy::Reject,
    );
    true
}

fn send_tunnel_error(usart: &Usart, seq: u16, code: rcard_usb_proto::messages::TunnelErrorCode) {
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
        FRAME_BUF
            .with(|fb| {
                if !fb.staged {
                    return Err(HostTransportError::NoPendingRequest);
                }
                if buf.len() < fb.staged_len {
                    return Err(HostTransportError::LeaseTooSmall);
                }
                let _ = buf.write_range(0, &fb.buf[..fb.staged_len]);
                Ok(fb.staged_len as u32)
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

        // COBS-encode the reply ([TYPE_IPC_REPLY || lease data]) directly
        // into FRAME_BUF, reading the lease in small stack-local chunks.
        // FRAME_BUF is free at this point — fetch_pending_request already
        // copied the staged data out.
        let result = FRAME_BUF.with(|fb| {
            let mut encoder = cobs::CobsEncoder::new(&mut fb.buf);

            // Type prefix byte.
            encoder
                .push(&[rcard_log::wire::TYPE_IPC_REPLY])
                .map_err(|_| ())?;

            // Lease body in 256-byte chunks.
            let mut scratch = [0u8; 256];
            let mut offset = 0;
            while offset < len {
                let chunk_len = (len - offset).min(256);
                let _ = buf.read_range(offset, &mut scratch[..chunk_len]);
                encoder.push(&scratch[..chunk_len]).map_err(|_| ())?;
                offset += chunk_len;
            }

            let enc_len = encoder.finalize();
            let _ = usart.write(&fb.buf[..enc_len]);
            let _ = usart.write(&[0x00]); // COBS delimiter
            Ok(())
        });

        // Clear the staged flag; dispatch is complete.
        FRAME_BUF.with(|fb| {
            fb.staged = false;
            fb.staged_len = 0;
        });

        result
            .unwrap_or(Err(()))
            .map_err(|_| HostTransportError::WireWriteFailed)
    }
}
