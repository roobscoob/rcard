#![no_std]
#![no_main]

use generated::peers::PEERS;
use generated::slots::SLOTS;
use once_cell::GlobalState;
use rcard_log::{error, info, warn, OptionExt};
use rcard_usb_proto::ipc_request::LeaseKind;
use rcard_usb_proto::tunnel::TunnelBuffer;

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

// Each binding lives in its own module because the macro generates a
// struct with a fixed name (`HostTransportServer`). Two invocations in
// the same scope would collide.
mod usb_transport_binding {
    sysmodule_host_transport_api::bind_host_transport!(UsbTransport = super::USB_TRANSPORT_TID);
}
use usb_transport_binding::UsbTransport;

mod usart_transport_binding {
    sysmodule_host_transport_api::bind_host_transport!(UsartTransport = super::USART_TRANSPORT_TID);
}
use usart_transport_binding::UsartTransport;

const LEASE_POOL_SIZE: usize = rcard_usb_proto::LEASE_POOL_SIZE;
const MAX_MESSAGE: usize = 256;
const MAX_LEASES: usize = rcard_usb_proto::MAX_LEASES;

// ---------------------------------------------------------------------------
// Shared tunnel buffer
// ---------------------------------------------------------------------------

#[unsafe(link_section = ".tunnel")]
static TUNNEL: TunnelBuffer = TunnelBuffer::new();

fn tunnel() -> &'static TunnelBuffer {
    &TUNNEL
}

static mut MSG_BUF: [u8; MAX_MESSAGE] = [0u8; MAX_MESSAGE];

struct LastDelivery {
    transport: u8,
    len: usize,
}

static LAST_DELIVERY: GlobalState<LastDelivery> = GlobalState::new(LastDelivery {
    transport: 0,
    len: 0,
});

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

/// Dispatch one wire frame using a single shared buffer.
///
/// Phases (all sequential, non-overlapping within `buf`):
///   1. Parse the fetch frame already in `buf[..fetch_len]`
///   2. Copy args to stack, compact lease payloads in-place in `buf`
///   3. sys_send with leases pointing into `buf`
///   4. Gather writebacks to the tail of `buf`, split_at_mut, encode
///      the reply into the head
fn dispatch_tunneled_request(
    buf: &mut [u8],
    fetch_len: usize,
) -> DispatchOutcome {
    // ── 1. Parse directly from `buf`. ──────────────────────────────────
    //
    // The frame is already fully assembled by usb_protocol_host before
    // being handed to us, so we don't need a FrameReader (which would
    // copy bytes into its own buffer and leave `view.lease_data(i)`
    // pointing outside `buf`, breaking the offset math in the compact
    // pass below).
    let Ok(header) = rcard_usb_proto::FrameHeader::decode(&buf[..fetch_len]) else {
        warn!("host_proxy: bad frame header");
        return DispatchOutcome::Skip;
    };
    let frame_total = header.frame_size();
    if frame_total > fetch_len {
        warn!("host_proxy: truncated frame");
        return DispatchOutcome::Skip;
    }
    let request_seq = header.seq;

    // ── 2. Copy args to stack, record lease layout ─────────────────────
    //
    // The view borrows `buf` immutably. We pull everything we need out
    // into owned locals inside an inner scope so the borrow ends before
    // the compact pass below re-borrows `buf` mutably. Bad-lease errors
    // are recorded as a flag and encoded after the scope ends, for the
    // same reason.
    let msg_buf = unsafe { &mut *(&raw mut MSG_BUF) };
    let mut slots: [LeaseSlot; MAX_LEASES] = [
        LeaseSlot { kind: LeaseKind::Read, offset: 0, length: 0 },
        LeaseSlot { kind: LeaseKind::Read, offset: 0, length: 0 },
        LeaseSlot { kind: LeaseKind::Read, offset: 0, length: 0 },
        LeaseSlot { kind: LeaseKind::Read, offset: 0, length: 0 },
    ];
    let mut src_offsets: [(usize, usize); MAX_LEASES] = [(0, 0); MAX_LEASES];

    let (target, opcode, lease_count, args_len, bad_lease) = {
        let frame = rcard_usb_proto::RawFrame {
            header,
            payload: &buf[rcard_usb_proto::HEADER_SIZE..frame_total],
        };
        let Some(view) = frame.as_ipc_request() else {
            warn!("host_proxy: non-IpcRequest frame");
            return DispatchOutcome::Skip;
        };

        let target = ipc::kern::TaskId::from(view.task_id);
        let opcode = ipc::opcode(view.resource_kind, view.method);
        let lease_count = view.lease_count().min(MAX_LEASES);

        let args_data = view.args();
        let args_len = args_data.len().min(MAX_MESSAGE);
        msg_buf[..args_len].copy_from_slice(&args_data[..args_len]);

        // Record where each lease's data lives in the original frame, so
        // the compact pass can copy it forward into buf[0..pool_end].
        // Source offsets are always ahead of the destination cursor
        // (frame header + args + descriptors precede lease data), so the
        // forward copy is safe.
        let mut bad_lease = false;
        for i in 0..lease_count {
            let Some(desc) = view.lease(i) else {
                bad_lease = true;
                break;
            };
            let len = desc.length as usize;
            if desc.kind.has_request_data() {
                if let Some(data) = view.lease_data(i) {
                    let frame_offset = data.as_ptr() as usize - buf.as_ptr() as usize;
                    src_offsets[i] = (frame_offset, data.len());
                }
            }
            slots[i] = LeaseSlot { kind: desc.kind, offset: 0, length: len };
        }

        (target, opcode, lease_count, args_len, bad_lease)
    };

    if bad_lease {
        let n = encode_tunnel_error(
            request_seq,
            rcard_usb_proto::messages::TunnelErrorCode::LeasePoolFull,
            buf,
        );
        return DispatchOutcome::Error { len: n };
    }

    // Compact lease data into buf[0..pool_end].
    let mut pool_end = 0usize;
    for i in 0..lease_count {
        let len = slots[i].length;
        if pool_end + len > LEASE_POOL_SIZE {
            let n = encode_tunnel_error(
                request_seq,
                rcard_usb_proto::messages::TunnelErrorCode::LeasePoolFull,
                buf,
            );
            return DispatchOutcome::Error { len: n };
        }

        slots[i].offset = pool_end;

        if slots[i].kind.has_request_data() {
            let (src_off, src_len) = src_offsets[i];
            let copy_len = src_len.min(len);
            buf.copy_within(src_off..src_off + copy_len, pool_end);
        } else {
            for b in &mut buf[pool_end..pool_end + len] {
                *b = 0;
            }
        }

        pool_end += len;
    }

    // ── 3. Build leases and call sys_send ──────────────────────────────
    // msg_buf already holds args from phase 2; sys_send reads args
    // then overwrites with the reply in-place.

    let send_result = {
        let mut remaining = &mut buf[..pool_end];
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
            msg_buf,
            args_len,
            &mut leases[..lease_count],
        )
    };

    // ── 4. Gather writebacks to tail, encode reply into head ───────────
    match send_result {
        Ok((rc, reply_len)) => {
            // Gather write-lease regions to the tail of buf. Iterate in
            // reverse so rightward copies don't clobber source data.
            let mut wb_total = 0usize;
            for i in 0..lease_count {
                if slots[i].kind.has_reply_data() {
                    wb_total += slots[i].length;
                }
            }

            let buf_len = buf.len();
            let wb_start = buf_len - wb_total;
            let mut wb_cursor = buf_len;
            for i in (0..lease_count).rev() {
                if slots[i].kind.has_reply_data() {
                    let len = slots[i].length;
                    wb_cursor -= len;
                    let src = slots[i].offset;
                    buf.copy_within(src..src + len, wb_cursor);
                }
            }

            let effective_reply_len = reply_len.max(1);

            // split_at_mut keeps the borrow checker happy: reply encoding
            // writes into head, reads writeback from tail.
            let (reply_buf, wb_buf) = buf.split_at_mut(wb_start);

            let reply = rcard_usb_proto::IpcReply {
                rc: rc.0,
                return_value: &msg_buf[..effective_reply_len],
                lease_writeback: &wb_buf[..wb_total],
            };
            match reply.encode_into(reply_buf, request_seq) {
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
                buf,
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

fn revoke_all_handles() {
    let mut msg = [0u8; 1];
    for task_index in 0..generated::tasks::TASK_COUNT {
        let target = ipc::kern::TaskId::gen0(task_index as u16);
        let opcode = ipc::opcode(0, ipc::REVOKE_ALL_METHOD);
        let _ = ipc::kern::sys_send(target, opcode, &mut msg, 0, &mut []);
    }
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
fn handle_host_request(sender: u16, code: u32) {
    let Some(transport) = select_transport(sender) else {
        warn!("host_proxy: host_request from unknown sender {}", sender);
        return;
    };

    if code == 0xFFFF_FFFF {
        warn!("host_proxy: transport disconnected, revoking all handles");
        revoke_all_handles();
        return;
    }

    if code == 1 {
        // ReplyCorrupted — retransmit the last reply without re-dispatching.
        let (last_transport, last_len) = LAST_DELIVERY
            .with(|ld| (ld.transport, ld.len))
            .unwrap_or((0, 0));
        if last_len == 0 {
            warn!("host_proxy: redeliver requested but no cached reply");
            return;
        }
        let expected_transport = match transport {
            Transport::Usb => 1u8,
            Transport::Usart => 2u8,
        };
        if last_transport != expected_transport {
            warn!("host_proxy: redeliver transport mismatch");
            return;
        }
        let tun = tunnel();
        // Transfer ownership to the transport for the redeliver.
        let transport_tid = tun.holder();
        info!("host_proxy: redelivering {} bytes", last_len);
        unsafe { tun.set_len(last_len as u32) };
        tun.transfer(transport_tid);
        let deliver_result = match transport {
            Transport::Usb => UsbTransport::deliver_reply(&[]),
            Transport::Usart => UsartTransport::deliver_reply(&[]),
        };
        match deliver_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => error!("host_proxy: redeliver failed: {}", e),
            Err(e) => error!("host_proxy: redeliver IPC failed: {}", e),
        }
        return;
    }

    let t0 = userlib::sys_get_timer().now;
    let tun = tunnel();
    let tun_data = unsafe { tun.data_mut() };

    // 1. Fetch — synchronization barrier. Data is already in the
    //    shared tunnel (transport transferred ownership to us).
    let request_len = {
        let result = match transport {
            Transport::Usb => UsbTransport::fetch_pending_request(tun_data),
            Transport::Usart => UsartTransport::fetch_pending_request(tun_data),
        };
        match result {
            Ok(Ok(len)) => len as usize,
            Ok(Err(e)) => {
                error!("host_proxy: fetch_pending_request failed: {}", e);
                return;
            }
            Err(e) => {
                error!("host_proxy: fetch IPC failed: {}", e);
                return;
            }
        }
    };

    if request_len == 0 {
        return;
    }

    let t1 = userlib::sys_get_timer().now;

    // 2. Dispatch in-place in the shared tunnel buffer.
    let outcome = dispatch_tunneled_request(tun_data, request_len);

    let t2 = userlib::sys_get_timer().now;

    // 3. Hand the encoded reply back to the originating transport.
    match outcome {
        DispatchOutcome::Reply { len } | DispatchOutcome::Error { len } if len > 0 => {
            LAST_DELIVERY
                .with(|ld| {
                    ld.transport = match transport {
                        Transport::Usb => 1,
                        Transport::Usart => 2,
                    };
                    ld.len = len;
                })
                .log_unwrap();

            // Transfer ownership back to the transport for wire emission.
            unsafe { tun.set_len(len as u32) };
            let transport_tid = match transport {
                Transport::Usb => USB_TRANSPORT_TASK.map_or(0, |t| u16::from(t) as u32),
                Transport::Usart => USART_TRANSPORT_TASK.map_or(0, |t| u16::from(t) as u32),
            };
            tun.transfer(transport_tid);

            let deliver_result = match transport {
                Transport::Usb => UsbTransport::deliver_reply(&[]),
                Transport::Usart => UsartTransport::deliver_reply(&[]),
            };
            let t3 = userlib::sys_get_timer().now;
            info!(
                "host_proxy: fetch={}ms dispatch={}ms deliver={}ms total={}ms reply_len={}",
                t1 - t0, t2 - t1, t3 - t2, t3 - t0, len
            );
            match deliver_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => error!("host_proxy: deliver_reply failed: {}", e),
                Err(e) => error!("host_proxy: deliver IPC failed: {}", e),
            }
        }
        _ => {}
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
