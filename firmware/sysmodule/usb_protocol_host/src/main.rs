#![no_std]
#![no_main]

use generated::slots::SLOTS;
use once_cell::GlobalState;
use rcard_log::{error, info, warn, ResultExt};
use rcard_usb_proto::ipc_request::LeaseKind;
use sysmodule_usb_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

sysmodule_usb_api::bind_usb_endpoint!(UsbEndpoint = SLOTS.sysmodule_usb);
sysmodule_usb_protocol_api::bind_usb_protocol_manager!(
    UsbProtocolManager = SLOTS.sysmodule_usb_protocol
);

const LEASE_POOL_SIZE: usize = rcard_usb_proto::LEASE_POOL_SIZE;
const MAX_MESSAGE: usize = 256;
const MAX_LEASES: usize = 4;

// ---------------------------------------------------------------------------
// Static buffers
//
// The tunnel is synchronous — one IPC request at a time. These statics
// are only accessed from handle_request(), which is never reentrant.
// ---------------------------------------------------------------------------

/// Lease data pool. Before sys_send, Read/ReadWrite data is copied in.
/// After sys_send, Write/ReadWrite regions contain the server's writeback.
static POOL: GlobalState<[u8; LEASE_POOL_SIZE]> = GlobalState::new([0u8; LEASE_POOL_SIZE]);

/// Staging area for the writeback data that the IpcReply encoder needs
/// as a single contiguous slice.
static WRITEBACK: GlobalState<[u8; LEASE_POOL_SIZE]> = GlobalState::new([0u8; LEASE_POOL_SIZE]);

/// Frame encoding buffer.
/// Max frame: header(5) + rc(4) + reply_len(1) + return_value(256) + writeback(8192) = 8458.
static FRAME_BUF: GlobalState<[u8; 8704]> = GlobalState::new([0u8; 8704]);

// ---------------------------------------------------------------------------
// USB write helper
// ---------------------------------------------------------------------------

fn write_usb(ep_in: &UsbEndpoint, data: &[u8]) {
    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + 64).min(data.len());
        match ep_in.write(&data[offset..end]) {
            Ok(Ok(n)) => offset += n as usize,
            Ok(Err(UsbError::EndpointBusy)) => continue,
            Ok(Err(e)) => {
                error!("USB write: {}", e);
                return;
            }
            Err(e) => {
                error!("USB IPC: {}", e);
                return;
            }
        }
    }
}

fn send_tunnel_error(seq: u16, code: rcard_usb_proto::messages::TunnelErrorCode, ep: &UsbEndpoint) {
    let msg = rcard_usb_proto::messages::TunnelError { code };
    let mut buf = [0u8; 16];
    if let Some(n) = rcard_usb_proto::simple::encode_simple(&msg, &mut buf, seq) {
        write_usb(ep, &buf[..n]);
    }
}

// ---------------------------------------------------------------------------
// IPC tunnel core
// ---------------------------------------------------------------------------

/// Per-lease bookkeeping — records where each lease lives in the pool
/// so we can find it again after sys_send for writeback.
struct LeaseSlot {
    kind: LeaseKind,
    offset: usize,
    length: usize,
}

fn handle_request(
    view: &rcard_usb_proto::IpcRequestView<'_>,
    request_seq: u16,
    ep_in: &UsbEndpoint,
) {
    let target = ipc::kern::TaskId::from(view.task_id);
    let opcode = ipc::opcode(view.resource_kind, view.method);
    let lease_count = view.lease_count().min(MAX_LEASES);

    // ── 1. Copy args to stack ───────────────────────────────────────
    //
    // IPC args wire format:
    //   Constructor/static: [param0][param1]...  (zerocopy, sequential)
    //   Instance method:    [handle:u64][param0][param1]...
    //
    // The host constructs args in exactly this format. We forward
    // verbatim — the target server's dispatcher deserializes them.

    let mut args = [0u8; MAX_MESSAGE];
    let args_data = view.args();
    let args_len = args_data.len().min(MAX_MESSAGE);
    args[..args_len].copy_from_slice(&args_data[..args_len]);

    // ── 2. Populate lease pool ──────────────────────────────────────
    //
    // Walk the lease descriptors and record each lease's position.
    // Copy Read/ReadWrite data from the USB frame into the pool.
    // Write lease regions are zeroed (server fills them via sys_borrow_write).

    let mut slots: [LeaseSlot; MAX_LEASES] = [
        LeaseSlot { kind: LeaseKind::Read, offset: 0, length: 0 },
        LeaseSlot { kind: LeaseKind::Read, offset: 0, length: 0 },
        LeaseSlot { kind: LeaseKind::Read, offset: 0, length: 0 },
        LeaseSlot { kind: LeaseKind::Read, offset: 0, length: 0 },
    ];
    let mut pool_end = 0usize;

    let pool_ok = POOL.with(|pool| {
        for i in 0..lease_count {
            let Some(desc) = view.lease(i) else { return false };
            let len = desc.length as usize;

            if pool_end + len > LEASE_POOL_SIZE {
                return false;
            }

            slots[i] = LeaseSlot {
                kind: desc.kind,
                offset: pool_end,
                length: len,
            };

            if desc.kind.has_request_data() {
                if let Some(data) = view.lease_data(i) {
                    pool[pool_end..pool_end + data.len()].copy_from_slice(data);
                }
            } else {
                // Zero the Write lease region.
                for b in &mut pool[pool_end..pool_end + len] {
                    *b = 0;
                }
            }

            pool_end += len;
        }
        true
    });

    if pool_ok != Some(true) {
        send_tunnel_error(request_seq, rcard_usb_proto::messages::TunnelErrorCode::LeasePoolFull, ep_in);
        return;
    }

    // ── 3. Build leases and call sys_send ───────────────────────────
    //
    // The pool is split into non-overlapping regions via split_at_mut.
    // Each region becomes a Lease with the correct access mode.
    //
    // The Lease<'a> lifetime ties to the pool borrow. After sys_send
    // returns, leases go out of scope (end of the block), releasing
    // the borrow. The pool can then be read for writeback.
    //
    // sys_send wire format:
    //   target:    TaskId (packed u16: 10-bit index + 6-bit generation)
    //   operation: u16 = (resource_kind << 8) | method
    //   outgoing:  &[u8] — the args buffer
    //   incoming:  &mut [u8] — reply buffer, kernel writes server's reply here
    //   leases:    &mut [Lease] — memory regions the server can borrow
    //
    // Returns Ok((ResponseCode, reply_len)) or Err(TaskDeath).
    //
    // Reply body format (written by server into incoming buf):
    //   [tag: u8]  0 = Ok, 1 = Err(ipc::Error)
    //   [payload]  return value (Ok) or ipc::Error byte (Err)
    //
    // ResponseCode::SUCCESS (0) = normal reply.
    // Non-zero rc (ACCESS_VIOLATION=2, MALFORMED_MESSAGE=1) = server
    //   rejected the message. Reply body may be empty (len=0).

    let mut reply_buf = [0u8; MAX_MESSAGE];

    let send_result = POOL.with(|pool| {
        let mut remaining = &mut pool[..pool_end];
        let mut leases: [ipc::kern::Lease; MAX_LEASES] = [
            ipc::kern::Lease::no_access(&[]),
            ipc::kern::Lease::no_access(&[]),
            ipc::kern::Lease::no_access(&[]),
            ipc::kern::Lease::no_access(&[]),
        ];

        for i in 0..lease_count {
            let len = slots[i].length;
            let (region, rest) = remaining.split_at_mut(len);
            remaining = rest;

            leases[i] = match slots[i].kind {
                LeaseKind::Read => ipc::kern::Lease::read_only(&*region),
                LeaseKind::Write | LeaseKind::ReadWrite => {
                    ipc::kern::Lease::read_write(region)
                }
            };
        }

        ipc::kern::sys_send(
            target,
            opcode,
            &args[..args_len],
            &mut reply_buf,
            &mut leases[..lease_count],
        )
        // leases drop here, releasing pool borrows
    });

    let Some(send_result) = send_result else {
        send_tunnel_error(request_seq, rcard_usb_proto::messages::TunnelErrorCode::Internal, ep_in);
        return;
    };

    // ── 4. Encode reply ─────────────────────────────────────────────

    match send_result {
        Ok((rc, reply_len)) => {
            // Collect writeback data from the pool into a contiguous
            // buffer. Write/ReadWrite regions were modified in-place by
            // the server via sys_borrow_write — the pool IS the writeback.
            let wb_len = POOL.with(|pool| {
                WRITEBACK.with(|wb| {
                    let mut cursor = 0usize;
                    for i in 0..lease_count {
                        if slots[i].kind.has_reply_data() {
                            let off = slots[i].offset;
                            let len = slots[i].length;
                            wb[cursor..cursor + len].copy_from_slice(&pool[off..off + len]);
                            cursor += len;
                        }
                    }
                    cursor
                })
            });

            let Some(wb_len) = wb_len.flatten() else {
                send_tunnel_error(request_seq, rcard_usb_proto::messages::TunnelErrorCode::Internal, ep_in);
                return;
            };

            // Ensure return_value is at least 1 byte for the wire encoding.
            // reply_len is normally >= 1 (server sends at least the Ok/Err tag).
            // For non-SUCCESS rc, reply_len may be 0 — pad with a zero byte.
            let effective_reply_len = reply_len.max(1);

            WRITEBACK.with(|wb| {
                FRAME_BUF.with(|frame_buf| {
                    let reply = rcard_usb_proto::IpcReply {
                        rc: rc.0,
                        return_value: &reply_buf[..effective_reply_len],
                        lease_writeback: &wb[..wb_len],
                    };

                    if let Some(n) = reply.encode_into(frame_buf, request_seq) {
                        write_usb(ep_in, &frame_buf[..n]);
                    }
                });
            });
        }
        Err(_task_death) => {
            send_tunnel_error(
                request_seq,
                rcard_usb_proto::messages::TunnelErrorCode::TaskDead,
                ep_in,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[export_name = "main"]
fn main() -> ! {
    info!("Awake");

    let handles = UsbProtocolManager::take_host_handles()
        .log_expect("manager IPC failed")
        .log_expect("take_host_handles failed");

    info!("Opening host channel endpoints");

    let ep_out = UsbEndpoint::open(
        handles.ep_out,
        EndpointConfig {
            number: 1,
            direction: Direction::Out,
            transfer_type: TransferType::Bulk,
            max_packet_size: 64,
            interval: 0,
        },
    )
    .log_expect("EP OUT IPC failed")
    .log_expect("EP OUT open failed");

    let ep_in = UsbEndpoint::open(
        handles.ep_in,
        EndpointConfig {
            number: 1,
            direction: Direction::In,
            transfer_type: TransferType::Bulk,
            max_packet_size: 64,
            interval: 0,
        },
    )
    .log_expect("EP IN IPC failed")
    .log_expect("EP IN open failed");

    // Poll until USB is configured. Each bus_state() call drives one
    // round of USB enumeration in the USB sysmodule.
    info!("Waiting for USB configuration");
    loop {
        match UsbProtocolManager::bus_state() {
            Ok(BusState::Configured) => break,
            _ => {}
        }
    }

    info!("Host tunnel ready");

    let mut reader = rcard_usb_proto::FrameReader::<4096>::new();
    let mut usb_buf = [0u8; 64];

    loop {
        // Poll USB OUT. Each read() call also drives USB polling inside
        // the USB sysmodule, keeping the bus alive.
        match ep_out.read(&mut usb_buf) {
            Ok(Ok(n)) if n > 0 => {
                reader.push(&usb_buf[..n as usize]);
            }
            Ok(Err(UsbError::Disconnected)) => {
                warn!("USB disconnected");
                reader.reset();
                loop {
                    match UsbProtocolManager::bus_state() {
                        Ok(BusState::Configured) => break,
                        _ => {}
                    }
                }
                info!("USB reconnected");
                continue;
            }
            _ => continue,
        }

        // Drain complete frames.
        loop {
            match reader.next_frame() {
                Ok(Some(frame)) => {
                    let size = frame.header.frame_size();
                    if let Some(req) = frame.as_ipc_request() {
                        handle_request(&req, frame.header.seq, &ep_in);
                    } else {
                        warn!("Unexpected frame type on host channel");
                    }
                    reader.consume(size);
                }
                Ok(None) => break,
                Err(rcard_usb_proto::ReaderError::Oversized { declared_size }) => {
                    warn!("Oversized frame, skipping");
                    reader.skip_frame(declared_size);
                }
                Err(rcard_usb_proto::ReaderError::Header(_)) => {
                    error!("Bad frame header, resetting");
                    reader.reset();
                    break;
                }
            }
        }
    }
}
