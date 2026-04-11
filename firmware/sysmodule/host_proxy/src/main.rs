#![no_std]
#![no_main]

use generated::peers::PEERS;
use generated::slots::SLOTS;
use once_cell::GlobalState;
use rcard_log::{error, info, warn, OptionExt, ResultExt};
use rcard_usb_proto::ipc_request::LeaseKind;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log; cleanup Reactor);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

// Transports are resolved through `peers`, not `depends_on` — they
// deliberately do not appear as outgoing edges from host_proxy in the
// dependency graph. Cross-task ACL is granted via the app-level
// `trusted_senders` entry that marks host_proxy as trusted. Missing
// peers are tolerated: the task index const falls back to u16::MAX,
// which will never match a real sender, so the corresponding dispatch
// arm is effectively inert.
const USB_TRANSPORT_TASK: Option<ipc::kern::TaskId> = PEERS.sysmodule_usb_protocol_host;
const USART_TRANSPORT_TASK: Option<ipc::kern::TaskId> = PEERS.sysmodule_log;

const USB_TRANSPORT_INDEX: u16 = match USB_TRANSPORT_TASK {
    Some(tid) => tid.task_index(),
    None => u16::MAX,
};
const USART_TRANSPORT_INDEX: u16 = match USART_TRANSPORT_TASK {
    Some(tid) => tid.task_index(),
    None => u16::MAX,
};

// Each transport gets its own alias. The `bind_host_transport!` macro
// requires a const `TaskId`, so for absent peers we fall back to an
// unreachable sentinel. `select_transport` below refuses to return a
// variant whose peer is absent, so the sentinel TaskId can never be
// used in an actual IPC — and `main()` startup panics if *no*
// transport peers are present at all, because a host_proxy with no
// wire connection is a build-configuration error we'd rather catch
// loudly than let boot silently.
const ABSENT_PEER_SENTINEL: ipc::kern::TaskId = ipc::kern::TaskId::gen0(u16::MAX);

const USB_TRANSPORT_TID: ipc::kern::TaskId = match USB_TRANSPORT_TASK {
    Some(tid) => tid,
    None => ABSENT_PEER_SENTINEL,
};
const USART_TRANSPORT_TID: ipc::kern::TaskId = match USART_TRANSPORT_TASK {
    Some(tid) => tid,
    None => ABSENT_PEER_SENTINEL,
};

sysmodule_host_transport_api::bind_host_transport!(UsbTransport = USB_TRANSPORT_TID);
sysmodule_host_transport_api::bind_host_transport!(UsartTransport = USART_TRANSPORT_TID);

const LEASE_POOL_SIZE: usize = rcard_usb_proto::LEASE_POOL_SIZE;
const MAX_MESSAGE: usize = 256;
const MAX_LEASES: usize = 4;
const MAX_FRAME: usize = 8704;

// ---------------------------------------------------------------------------
// Static buffers
//
// The tunnel is synchronous — one IPC request at a time. These statics are
// only accessed from handle_host_request(), which is never reentrant (it
// runs inside the reactor-wake closure of the server loop).
// ---------------------------------------------------------------------------

/// Staged incoming request. Populated from `Transport::fetch_pending_request`
/// before dispatch.
static FETCH_BUF: GlobalState<[u8; MAX_FRAME]> = GlobalState::new([0u8; MAX_FRAME]);

/// Encoded outgoing reply. Handed back to the transport via
/// `Transport::deliver_reply` after dispatch.
static REPLY_BUF: GlobalState<[u8; MAX_FRAME]> = GlobalState::new([0u8; MAX_FRAME]);

/// Lease data pool. Before sys_send, Read/ReadWrite lease payloads are
/// copied in from the incoming request. After sys_send, Write/ReadWrite
/// regions contain the target server's writeback.
static POOL: GlobalState<[u8; LEASE_POOL_SIZE]> = GlobalState::new([0u8; LEASE_POOL_SIZE]);

/// Staging area for writeback data. `IpcReply` needs the writeback
/// payloads as a single contiguous slice, so after sys_send we gather
/// the Write/ReadWrite regions out of POOL in order.
static WRITEBACK: GlobalState<[u8; LEASE_POOL_SIZE]> = GlobalState::new([0u8; LEASE_POOL_SIZE]);

// ---------------------------------------------------------------------------
// Tunnel dispatch core (moved verbatim from the old usb_protocol_host)
// ---------------------------------------------------------------------------

/// Per-lease bookkeeping — records where each lease lives in the pool so
/// we can find it again after sys_send for writeback collection.
struct LeaseSlot {
    kind: LeaseKind,
    offset: usize,
    length: usize,
}

/// Outcome of a tunneled dispatch. The caller delivers the encoded reply
/// or tunnel error via `Transport::deliver_reply`.
enum DispatchOutcome {
    /// `reply_buf[..len]` holds an encoded `IpcReply` frame ready for the wire.
    Reply { len: usize },
    /// `reply_buf[..len]` holds an encoded `TunnelError` frame ready for the wire.
    Error { len: usize },
    /// Frame couldn't even be parsed — nothing to send back.
    Skip,
}

fn encode_tunnel_error(
    seq: u16,
    code: rcard_usb_proto::messages::TunnelErrorCode,
    reply_buf: &mut [u8],
) -> usize {
    let msg = rcard_usb_proto::messages::TunnelError { code };
    rcard_usb_proto::simple::encode_simple(&msg, reply_buf, seq).unwrap_or(0)
}

/// Dispatch one wire frame. Parses the `IpcRequest`, forwards it via
/// `sys_send` to the target, encodes the `IpcReply`, and writes the
/// encoded bytes into `reply_buf`. Returns a `DispatchOutcome` telling the
/// caller how many bytes of `reply_buf` to hand to `deliver_reply`.
fn dispatch_tunneled_request(
    fetch_bytes: &[u8],
    reply_buf: &mut [u8; MAX_FRAME],
) -> DispatchOutcome {
    // Re-parse the frame out of FETCH_BUF. The transport validated it
    // as an IpcRequest before staging it, but we do the framed decode
    // ourselves here so we don't have to marshal parsed references
    // across an IPC boundary.
    let mut reader = rcard_usb_proto::FrameReader::<{ MAX_FRAME }>::new();
    reader.push(fetch_bytes);

    let (view_seq, view_len) = match reader.next_frame() {
        Ok(Some(frame)) => {
            if frame.as_ipc_request().is_none() {
                warn!("host_proxy: non-IpcRequest frame");
                return DispatchOutcome::Skip;
            }
            (frame.header.seq, frame.header.frame_size())
        }
        _ => {
            warn!("host_proxy: frame reader failed");
            return DispatchOutcome::Skip;
        }
    };

    // Re-fetch the frame now that we've committed to processing it —
    // next_frame borrows the reader, so we can't hold `frame` across
    // reader operations.
    let Ok(Some(frame)) = reader.next_frame() else {
        return DispatchOutcome::Skip;
    };
    let Some(view) = frame.as_ipc_request() else {
        return DispatchOutcome::Skip;
    };
    let _ = view_len;
    let request_seq = view_seq;

    let target = ipc::kern::TaskId::from(view.task_id);
    let opcode = ipc::opcode(view.resource_kind, view.method);
    let lease_count = view.lease_count().min(MAX_LEASES);

    // ── 1. Copy args to stack ──────────────────────────────────────────
    let mut args = [0u8; MAX_MESSAGE];
    let args_data = view.args();
    let args_len = args_data.len().min(MAX_MESSAGE);
    args[..args_len].copy_from_slice(&args_data[..args_len]);

    // ── 2. Populate lease pool ─────────────────────────────────────────
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
                for b in &mut pool[pool_end..pool_end + len] {
                    *b = 0;
                }
            }

            pool_end += len;
        }
        true
    });

    if pool_ok != Some(true) {
        let n = encode_tunnel_error(
            request_seq,
            rcard_usb_proto::messages::TunnelErrorCode::LeasePoolFull,
            reply_buf,
        );
        return DispatchOutcome::Error { len: n };
    }

    // ── 3. Build leases and call sys_send ──────────────────────────────
    let mut kernel_reply = [0u8; MAX_MESSAGE];

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
            &mut kernel_reply,
            &mut leases[..lease_count],
        )
    });

    let Some(send_result) = send_result else {
        let n = encode_tunnel_error(
            request_seq,
            rcard_usb_proto::messages::TunnelErrorCode::Internal,
            reply_buf,
        );
        return DispatchOutcome::Error { len: n };
    };

    // ── 4. Encode reply ────────────────────────────────────────────────
    match send_result {
        Ok((rc, reply_len)) => {
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
                let n = encode_tunnel_error(
                    request_seq,
                    rcard_usb_proto::messages::TunnelErrorCode::Internal,
                    reply_buf,
                );
                return DispatchOutcome::Error { len: n };
            };

            let effective_reply_len = reply_len.max(1);

            let encoded = WRITEBACK.with(|wb| {
                let reply = rcard_usb_proto::IpcReply {
                    rc: rc.0,
                    return_value: &kernel_reply[..effective_reply_len],
                    lease_writeback: &wb[..wb_len],
                };
                reply.encode_into(reply_buf, request_seq).unwrap_or(0)
            });

            match encoded {
                Some(n) if n > 0 => DispatchOutcome::Reply { len: n },
                _ => {
                    let n = encode_tunnel_error(
                        request_seq,
                        rcard_usb_proto::messages::TunnelErrorCode::Internal,
                        reply_buf,
                    );
                    DispatchOutcome::Error { len: n }
                }
            }
        }
        Err(_task_death) => {
            let n = encode_tunnel_error(
                request_seq,
                rcard_usb_proto::messages::TunnelErrorCode::TaskDead,
                reply_buf,
            );
            DispatchOutcome::Error { len: n }
        }
    }
}

// ---------------------------------------------------------------------------
// Notification handler
// ---------------------------------------------------------------------------

/// Which transport pushed the current `host_request` notification.
/// Drives the static dispatch to the right typed client in
/// `handle_host_request`.
#[derive(Copy, Clone)]
enum Transport {
    Usb,
    Usart,
}

fn select_transport(sender: u16) -> Option<Transport> {
    // Gate on `is_some()` of the original peer option so a build
    // without one of the transports can never route to the sentinel
    // TaskId stored in USB_TRANSPORT_TID / USART_TRANSPORT_TID.
    if USB_TRANSPORT_TASK.is_some() && sender == USB_TRANSPORT_INDEX {
        Some(Transport::Usb)
    } else if USART_TRANSPORT_TASK.is_some() && sender == USART_TRANSPORT_INDEX {
        Some(Transport::Usart)
    } else {
        None
    }
}

#[ipc::notification_handler(host_request)]
fn handle_host_request(sender: u16, _code: u32) {
    let Some(transport) = select_transport(sender) else {
        warn!("host_proxy: host_request from unknown sender {}", sender);
        return;
    };

    // 1. Fetch the staged request from the transport that fired.
    let request_len = FETCH_BUF
        .with(|fetch| {
            let result = match transport {
                Transport::Usb => UsbTransport::fetch_pending_request(fetch),
                Transport::Usart => UsartTransport::fetch_pending_request(fetch),
            };
            match result {
                Ok(Ok(len)) => len as usize,
                Ok(Err(e)) => {
                    error!("host_proxy: fetch_pending_request failed: {}", e);
                    0
                }
                Err(e) => {
                    error!("host_proxy: fetch IPC failed: {}", e);
                    0
                }
            }
        })
        .log_unwrap();

    if request_len == 0 {
        return;
    }

    // 2. Run the tunneled dispatch against the staged request, encoding
    //    the reply into REPLY_BUF.
    let outcome = FETCH_BUF
        .with(|fetch| {
            REPLY_BUF
                .with(|reply| dispatch_tunneled_request(&fetch[..request_len], reply))
                .log_unwrap()
        })
        .log_unwrap();

    // 3. Hand the encoded reply back to the originating transport.
    match outcome {
        DispatchOutcome::Reply { len } | DispatchOutcome::Error { len } if len > 0 => {
            REPLY_BUF
                .with(|reply| {
                    let deliver_result = match transport {
                        Transport::Usb => UsbTransport::deliver_reply(&reply[..len]),
                        Transport::Usart => UsartTransport::deliver_reply(&reply[..len]),
                    };
                    match deliver_result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => error!("host_proxy: deliver_reply failed: {}", e),
                        Err(e) => error!("host_proxy: deliver IPC failed: {}", e),
                    }
                })
                .log_unwrap();
        }
        _ => {
            // Skip — nothing to deliver.
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[export_name = "main"]
fn main() -> ! {
    // A host_proxy with no transport peers is inert — it will never
    // receive a `host_request` notification it can dispatch. Fail loud
    // at startup rather than running a silent zombie, since this
    // almost certainly means the app.ncl is misconfigured.
    if USB_TRANSPORT_TASK.is_none() && USART_TRANSPORT_TASK.is_none() {
        userlib::sys_panic(b"host_proxy: no transport peers configured");
    }

    info!("Awake");

    ipc::server! {
        @notifications(Reactor) => handle_host_request,
    }
}
