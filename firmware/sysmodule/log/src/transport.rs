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
//! The inbound decode path writes directly into the shared tunnel
//! buffer. The outbound path uses a streaming COBS encoder (255 bytes).

use generated::peers::PEERS;
use once_cell::GlobalState;
use rcard_usb_proto::tunnel::TunnelBuffer;
use sysmodule_host_transport_api::*;

use crate::{send_ipc_reply, Reactor, Usart, USART};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// True iff this firmware build includes a `sysmodule_host_proxy` task.
const HOST_FORWARDING_AVAILABLE: bool = PEERS.sysmodule_host_proxy.is_some();

const HOST_PROXY_TID: u32 = match PEERS.sysmodule_host_proxy {
    Some(tid) => tid.task_index() as u32,
    None => 0,
};

const SELF_TID: u32 = generated::tasks::TASK_ID_SYSMODULE_LOG as u32;

fn refresh_tid(tid: u32) -> u32 {
    u16::from(userlib::sys_refresh_task_id(userlib::TaskId::from(tid as u16))) as u32
}

// ---------------------------------------------------------------------------
// Shared tunnel buffer
// ---------------------------------------------------------------------------

#[unsafe(link_section = ".tunnel")]
static TUNNEL: TunnelBuffer = TunnelBuffer::new();

fn tunnel() -> &'static TunnelBuffer {
    &TUNNEL
}

// ---------------------------------------------------------------------------
// Transport-local decode state (not in shared memory)
// ---------------------------------------------------------------------------

struct DecodeState {
    write_pos: usize,
    staged_len: usize,
    staged: bool,
    truncated: bool,
    locked: bool,
}

impl DecodeState {
    const fn new() -> Self {
        Self {
            write_pos: 0,
            staged_len: 0,
            staged: false,
            truncated: false,
            locked: false,
        }
    }
}

static DECODE: GlobalState<DecodeState> = GlobalState::new(DecodeState::new());

/// Streaming COBS decoder state.
static DECODER: GlobalState<cobs::DecoderState> = GlobalState::new(cobs::DecoderState::Idle);

// ---------------------------------------------------------------------------
// Inbound byte processing
// ---------------------------------------------------------------------------

/// Drain the USART2 RX ring and feed bytes through the streaming COBS
/// decoder. On each complete frame, dispatch by type byte.
pub fn handle_usart_rx() {
    let Some(usart) = USART.get() else { return };

    let busy = DECODE.with(|ds| ds.staged).unwrap_or(false);
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
            DECODE.with(|ds| {
                if ds.truncated {
                    return;
                }
                if !ds.locked && !ds.staged {
                    if !tunnel().try_acquire_or_wipe(SELF_TID, refresh_tid) {
                        ds.truncated = true;
                        return;
                    }
                    ds.locked = true;
                }
                let tun_data = unsafe { tunnel().data_mut() };
                if ds.write_pos < tun_data.len() {
                    tun_data[ds.write_pos] = byte;
                    ds.write_pos += 1;
                } else {
                    ds.truncated = true;
                }
            });
        }
        Some(Ok(DecodeResult::DataComplete)) => {
            DECODE.with(|ds| {
                let decoded_len = ds.write_pos;
                let was_truncated = ds.truncated;
                ds.write_pos = 0;
                ds.truncated = false;

                if decoded_len == 0 || was_truncated {
                    if ds.locked {
                        tunnel().release();
                        ds.locked = false;
                    }
                    return;
                }

                let tun_data = unsafe { tunnel().data_mut() };
                let type_byte = tun_data[0];
                tun_data.copy_within(1..decoded_len, 0);
                let body_len = decoded_len - 1;

                match type_byte {
                    rcard_log::wire::TYPE_IPC_REQUEST => {
                        if stage_ipc_request(usart, &tun_data[..body_len]) {
                            ds.staged_len = body_len;
                            ds.staged = true;
                            unsafe { tunnel().set_len(body_len as u32) };
                            tunnel().transfer(HOST_PROXY_TID);
                        } else {
                            tunnel().release();
                            ds.locked = false;
                        }
                    }
                    rcard_log::wire::TYPE_CONTROL_REQUEST => {
                        handle_control_request(usart, &tun_data[..body_len]);
                        tunnel().release();
                        ds.locked = false;
                    }
                    _ => {
                        tunnel().release();
                        ds.locked = false;
                    }
                }
            });
        }
        Some(Ok(DecodeResult::NoData)) => {}
        Some(Err(_)) | None => {
            DECODE.with(|ds| {
                if ds.locked {
                    tunnel().release();
                    ds.locked = false;
                }
                ds.write_pos = 0;
                ds.truncated = false;
            });
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

    // Data is already in the tunnel buffer (placed by the streaming decoder).
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
// Streaming COBS encoder — flushes completed blocks to USART
//
// COBS encodes a stream of bytes such that 0x00 never appears in the
// output.  The encoding works in blocks of up to 254 data bytes:
//
//   [overhead_byte] [data_0] [data_1] ... [data_N-1]
//
// where overhead_byte = N+1 (distance to the next overhead byte or end),
// and none of data_0..data_N-1 are 0x00.  A 0x00 in the input ends the
// current block early.  A run of 254 non-zero bytes also ends a block.
//
// This encoder buffers a single block (≤255 bytes) and flushes it to
// the USART when the block is complete.  Total buffer: 255 bytes.
// ---------------------------------------------------------------------------

static mut COBS_BUF: [u8; 255] = [0u8; 255];

struct StreamingCobsEncoder<'a> {
    usart: &'a Usart,
    buf: &'a mut [u8; 255],
    count: u8,
}

impl<'a> StreamingCobsEncoder<'a> {
    fn new(usart: &'a Usart) -> Self {
        Self {
            usart,
            buf: unsafe { &mut *(&raw mut COBS_BUF) },
            count: 0,
        }
    }

    fn flush_block(&mut self, overhead: u8) {
        self.buf[0] = overhead;
        let len = 1 + self.count as usize;
        let _ = self.usart.write(&self.buf[..len]);
        self.count = 0;
    }

    fn push(&mut self, data: &[u8]) {
        for &byte in data {
            if byte == 0 {
                self.flush_block(self.count + 1);
            } else {
                self.buf[1 + self.count as usize] = byte;
                self.count += 1;
                if self.count == 254 {
                    self.flush_block(0xFF);
                }
            }
        }
    }

    fn finalize(mut self) {
        self.flush_block(self.count + 1);
        let _ = self.usart.write(&[0x00]);
    }
}

// ---------------------------------------------------------------------------
// HostTransport server impl
// ---------------------------------------------------------------------------

pub struct LogHostTransport;

impl HostTransport for LogHostTransport {
    fn fetch_pending_request(
        _meta: ipc::Meta,
        _buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<u32, HostTransportError> {
        DECODE
            .with(|ds| {
                if !ds.staged {
                    return Err(HostTransportError::NoPendingRequest);
                }
                Ok(ds.staged_len as u32)
            })
            .unwrap_or(Err(HostTransportError::NoPendingRequest))
    }

    fn deliver_reply(
        _meta: ipc::Meta,
        _buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<(), HostTransportError> {
        let Some(usart) = USART.get() else {
            return Err(HostTransportError::WireWriteFailed);
        };

        let tun = tunnel();
        let len = tun.get_len() as usize;
        let data = unsafe { &tun.data_ref()[..len] };

        {
            let mut encoder = StreamingCobsEncoder::new(usart);
            encoder.push(&[rcard_log::wire::TYPE_IPC_REPLY]);
            encoder.push(data);
            encoder.finalize();
        }

        DECODE.with(|ds| {
            ds.staged = false;
            ds.staged_len = 0;
            ds.locked = false;
        });
        tun.release();

        Ok(())
    }
}
