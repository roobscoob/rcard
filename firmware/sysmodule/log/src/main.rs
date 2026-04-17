#![no_std]
#![no_main]
#![allow(clippy::unwrap_used)]

use generated::slots::SLOTS;
use once_cell::OnceCell;
use sysmodule_host_transport_api::*;
use sysmodule_log_api::*;

mod ringbuf;
mod server;
mod transport;

sysmodule_usart_api::bind_usart!(Usart = SLOTS.sysmodule_usart);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);
sysmodule_efuse_api::bind_efuse!(Efuse = SLOTS.sysmodule_efuse);

pub(crate) static USART: OnceCell<Usart> = OnceCell::new();

/// Chip UID read once from eFuse bank 0 at task startup. Used by
/// `send_awake` to populate the `Awake` payload without re-IPCing the
/// efuse server on every control-request reply.
pub(crate) static CACHED_UID: OnceCell<[u8; 16]> = OnceCell::new();

pub(crate) fn usart_write(data: &[u8]) {
    if let Some(usart) = USART.get() {
        let _ = usart.write(data);
    }
}

/// Maximum body size for `send_ipc_reply`. Tunnel errors (~7 bytes),
/// Awake (~40 bytes), and MoshiMoshi replies fit easily. Full IPC
/// replies are handled by `deliver_reply` which COBS-encodes directly
/// into FRAME_BUF from the lease — never through this function.
const MAX_SMALL_REPLY: usize = 64;

/// Send a small IPC reply (tunnel error, Awake, etc.) wrapped in
/// TYPE_IPC_REPLY + COBS. Uses stack-local buffers only.
pub(crate) fn send_ipc_reply(usart: &Usart, body: &[u8]) -> Result<(), ()> {
    if body.len() > MAX_SMALL_REPLY {
        return Err(());
    }
    let mut raw = [0u8; 1 + MAX_SMALL_REPLY];
    raw[0] = rcard_log::wire::TYPE_IPC_REPLY;
    raw[1..1 + body.len()].copy_from_slice(body);
    let raw_len = 1 + body.len();

    let mut encoded = [0u8; cobs::max_encoding_length(1 + MAX_SMALL_REPLY) + 1];
    let enc_len = cobs::encode(&raw[..raw_len], &mut encoded);
    encoded[enc_len] = 0x00;
    let _ = usart.write(&encoded[..enc_len + 1]);
    Ok(())
}

/// Notification handler for `usart_event`. Runs when sysmodule_usart
/// reports RX data available; drives the COBS accumulator in
/// `transport`, which stages any complete IPC-request frames and wakes
/// `host_proxy`.
#[ipc::notification_handler(usart_event)]
fn handle_usart_event(_sender: u16, _code: u32) {
    transport::handle_usart_rx();
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    ipc::notify_dead!(Reactor);
    userlib::sys_panic(b"log panic")
}

#[export_name = "main"]
fn main() -> ! {
    let usart = Usart::open(2).unwrap().unwrap();
    USART.set(usart).ok();

    // Read the chip UID once at startup and cache it. `send_awake` will
    // consult this cache for both the boot-time sentinel and any later
    // `MoshiMoshi` replies from the control-request handler.
    CACHED_UID.set(read_chip_uid()).ok();

    // Announce ourselves on the control channel. The host uses this as
    // the authoritative "device is up" signal for USART2, independent of
    // whatever log traffic does or doesn't happen to be flowing.
    send_awake(0);

    ipc::server! {
        Log: server::LogResource,
        HostTransport: transport::LogHostTransport,
        @notifications(Reactor) => handle_usart_event,
    }
}

/// Encode an `Awake` simple frame and ship it out on TYPE_IPC_REPLY.
///
/// Carries the device's 16-byte chip UID (cached from eFuse bank 0 at
/// task startup) plus this firmware image's build id (parsed from the
/// generated `BUILD_ID_BYTES` const). The host uses UID for device
/// identity and build id to look up matching .tfw metadata.
///
/// `seq` is the frame sequence number — `0` for the boot-time sentinel,
/// or the request's seq when replying to a `MoshiMoshi` control ping so
/// the host can match response to request.
pub(crate) fn send_awake(seq: u16) {
    use rcard_usb_proto::messages::{Awake, AWAKE_PAYLOAD_SIZE};
    use rcard_usb_proto::{simple::encode_simple, HEADER_SIZE};

    let uid = CACHED_UID.get().copied().unwrap_or([0u8; 16]);
    let firmware_id = ::generated::build_info::BUILD_ID_BYTES;

    // header + opcode + payload
    const FRAME_LEN: usize = HEADER_SIZE + 1 + AWAKE_PAYLOAD_SIZE;
    let mut frame = [0u8; FRAME_LEN];

    let Some(n) = encode_simple(&Awake::new(uid, firmware_id), &mut frame, seq) else {
        panic!("failed to encode Awake frame");
    };

    let Some(usart) = USART.get() else {
        panic!("USART not initialized");
    };

    let _ = send_ipc_reply(usart, &frame[..n]);
}

/// Well-known opcode the supervisor accepts for forwarding a pre-formatted
/// text line to USART1. ACL'd on the supervisor side to this task only —
/// see `firmware/tasks/supervisor/src/main.rs::handle_emit_log` and the
/// `peers = ["sysmodule_log"]` entry in supervisor's `task.ncl`.
const OP_EMIT_LOG: u16 = 0xE10C;

/// Push a `hello uid=<32hex> build=<32hex>\r\n` line to USART1 by IPC'ing
/// the supervisor (which owns USART1 via `uses_peripherals = ["usart1"]`).
///
/// Used as a side-effect of the MoshiMoshi → Awake exchange on USART2: the
/// host gets the same identity payload on whichever wire it's listening
/// to. Best-effort — failures are silently dropped.
pub(crate) fn emit_supervisor_hello(uid: [u8; 16], firmware_id: [u8; 16]) {
    // Layout: "hello uid=" + 32 hex + " build=" + 32 hex + "\r\n" = 81 bytes.
    const PREFIX_UID: &[u8] = b"hello uid=";
    const PREFIX_BUILD: &[u8] = b" build=";
    const SUFFIX: &[u8] = b"\r\n";
    const LEN: usize = PREFIX_UID.len() + 32 + PREFIX_BUILD.len() + 32 + SUFFIX.len();

    let mut buf = [0u8; LEN];
    let mut i = 0;
    buf[i..i + PREFIX_UID.len()].copy_from_slice(PREFIX_UID);
    i += PREFIX_UID.len();
    write_hex32(&mut buf[i..i + 32], &uid);
    i += 32;
    buf[i..i + PREFIX_BUILD.len()].copy_from_slice(PREFIX_BUILD);
    i += PREFIX_BUILD.len();
    write_hex32(&mut buf[i..i + 32], &firmware_id);
    i += 32;
    buf[i..i + SUFFIX.len()].copy_from_slice(SUFFIX);

    // Address the supervisor at the well-known TaskId(0) — same convention
    // the reactor uses for OP_DROP_REPORT (see reactor::main.rs:57).
    let sup = userlib::TaskId::gen0(0);
    let _ = userlib::sys_send(sup, OP_EMIT_LOG, &buf, &mut [], &mut []);
}

/// Hex-encode `bytes` (16 bytes) into `out` (32 bytes), lowercase.
fn write_hex32(out: &mut [u8], bytes: &[u8; 16]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, &b) in bytes.iter().enumerate() {
        out[i * 2] = HEX[(b >> 4) as usize];
        out[i * 2 + 1] = HEX[(b & 0xF) as usize];
    }
}

/// Fetch eFuse bank 0 and return its first 16 bytes as the chip UID.
/// Returns zeros on any IPC failure — the awake sentinel still fires.
fn read_chip_uid() -> [u8; 16] {
    match Efuse::read(0) {
        Ok(Ok(bank)) => {
            let mut uid = [0u8; 16];
            uid.copy_from_slice(&bank[..16]);
            uid
        }
        _ => [0u8; 16],
    }
}
