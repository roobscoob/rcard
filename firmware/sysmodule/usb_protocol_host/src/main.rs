#![no_std]
#![no_main]

use generated::notifications;
use generated::peers::PEERS;
use generated::slots::SLOTS;
use once_cell::{GlobalState, OnceCell};
use rcard_usb_proto::tunnel::TunnelBuffer;
use rcard_log::{error, info, warn, OptionExt, ResultExt};
use sysmodule_host_transport_api::*;
use sysmodule_usb_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log; cleanup Reactor);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

sysmodule_usb_api::bind_usb_endpoint!(UsbEndpoint = SLOTS.sysmodule_usb);
sysmodule_usb_protocol_api::bind_usb_protocol_manager!(
    UsbProtocolManager = SLOTS.sysmodule_usb_protocol
);

use sysmodule_reactor_api::OverflowStrategy;

/// Per-packet CRC-16/CCITT-FALSE (poly=0x1021, init=0xFFFF, no reflect,
/// xorout=0). Used over the whole IPC frame on each bulk transfer.
fn crc16(data: &[u8]) -> u16 {
    crc16_update(0xFFFF, data)
}

fn crc16_update(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

// ---------------------------------------------------------------------------
// Shared tunnel buffer
// ---------------------------------------------------------------------------

#[unsafe(link_section = ".tunnel")]
static TUNNEL: TunnelBuffer = TunnelBuffer::new();

fn tunnel() -> &'static TunnelBuffer {
    &TUNNEL
}

const SELF_TID: u32 = generated::tasks::TASK_ID_SYSMODULE_USB_PROTOCOL_HOST as u32;

fn refresh_tid(tid: u32) -> u32 {
    u16::from(userlib::sys_refresh_task_id(userlib::TaskId::from(tid as u16))) as u32
}

// ---------------------------------------------------------------------------
// Transport-local accumulation state (not in shared memory)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum UsbTunnelState {
    Idle,
    Accumulating,
    Overflowed,
    Staged,
    Retransmitting,
}

struct AccumState {
    state: UsbTunnelState,
    len: usize,
}

static ACCUM: GlobalState<AccumState> = GlobalState::new(AccumState {
    state: UsbTunnelState::Idle,
    len: 0,
});

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

static EP_OUT: OnceCell<UsbEndpoint> = OnceCell::new();
static EP_IN: OnceCell<UsbEndpoint> = OnceCell::new();

// ---------------------------------------------------------------------------
// Last-sent tracking for ReplyCorrupted retransmit
// ---------------------------------------------------------------------------

enum LastSent {
    None,
    IpcReply,
    Error { buf: [u8; 19], len: usize },
}

static LAST_SENT: GlobalState<LastSent> = GlobalState::new(LastSent::None);

// ---------------------------------------------------------------------------
// USB write helper
// ---------------------------------------------------------------------------

fn write_usb(ep_in: &UsbEndpoint, data: &[u8]) -> Result<(), HostTransportError> {
    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + 64).min(data.len());
        match ep_in.write(&data[offset..end]) {
            Ok(Ok(n)) => offset += n as usize,
            Ok(Err(UsbError::EndpointBusy)) => {
                continue;
            }
            Ok(Err(e)) => {
                error!("USB write: {}", e);
                return Err(HostTransportError::WireWriteFailed);
            }
            Err(e) => {
                error!("USB IPC: {}", e);
                return Err(HostTransportError::WireWriteFailed);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// HostTransport server implementation
// ---------------------------------------------------------------------------

struct HostTransportImpl;

impl HostTransport for HostTransportImpl {
    fn fetch_pending_request(
        _meta: ipc::Meta,
        _buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<u32, HostTransportError> {
        ACCUM
            .with(|a| {
                if a.state != UsbTunnelState::Staged {
                    return Err(HostTransportError::NoPendingRequest);
                }
                Ok(tunnel().get_len())
            })
            .unwrap_or(Err(HostTransportError::NoPendingRequest))
    }

    fn deliver_reply(
        _meta: ipc::Meta,
        _buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<(), HostTransportError> {
        let Some(ep_in) = EP_IN.get() else {
            return Err(HostTransportError::WireWriteFailed);
        };

        let t0 = userlib::sys_get_timer().now;

        let tun = tunnel();
        let len = tun.get_len() as usize;
        let data = unsafe { &tun.data_ref()[..len] };

        let crc = crc16(data);

        let t1 = userlib::sys_get_timer().now;

        let mut result = Ok(());
        let full_chunks = len / 64;
        let tail_len = len - full_chunks * 64;

        'emit: {
            for i in 0..full_chunks {
                if let Err(e) = write_usb(ep_in, &data[i * 64..(i + 1) * 64]) {
                    result = Err(e);
                    break 'emit;
                }
            }

            if tail_len > 0 {
                let mut packet = [0u8; 66];
                packet[..tail_len].copy_from_slice(&data[full_chunks * 64..]);
                packet[tail_len] = (crc >> 8) as u8;
                packet[tail_len + 1] = crc as u8;
                let total = tail_len + 2;
                if total <= 64 {
                    if let Err(e) = write_usb(ep_in, &packet[..total]) {
                        result = Err(e);
                        break 'emit;
                    }
                    if total == 64 {
                        if let Err(e) = write_usb(ep_in, &[0u8]) {
                            result = Err(e);
                            break 'emit;
                        }
                    }
                } else {
                    if let Err(e) = write_usb(ep_in, &packet[..64]) {
                        result = Err(e);
                        break 'emit;
                    }
                    if let Err(e) = write_usb(ep_in, &packet[64..total]) {
                        result = Err(e);
                        break 'emit;
                    }
                }
            } else {
                let trailer = [(crc >> 8) as u8, crc as u8];
                if let Err(e) = write_usb(ep_in, &trailer) {
                    result = Err(e);
                    break 'emit;
                }
            }
        }

        let t2 = userlib::sys_get_timer().now;
        info!(
            "deliver_reply: crc={}ms emit={}ms len={}",
            t1 - t0,
            t2 - t1,
            len
        );

        LAST_SENT.with(|ls| {
            *ls = LastSent::IpcReply;
        });

        ACCUM.with(|a| {
            a.state = UsbTunnelState::Idle;
            a.len = 0;
        });
        tun.release();

        result
    }
}

// ---------------------------------------------------------------------------
// Notification handler: drain USB, stage frames, wake host_proxy
// ---------------------------------------------------------------------------

/// True iff this firmware build includes a `sysmodule_host_proxy` task.
/// When false, the transport accepts and drains USB OUT, but immediately
/// replies with `NoHostForwarding` to any `IpcRequest` instead of staging
/// it. Resolved at codegen time from the `peers` table — host_proxy is
/// declared as a peer in this task's `task.ncl` purely for this check.
const HOST_FORWARDING_AVAILABLE: bool = PEERS.sysmodule_host_proxy.is_some();
const HOST_PROXY_TID: u32 = match PEERS.sysmodule_host_proxy {
    Some(tid) => tid.task_index() as u32,
    None => 0,
};

/// Tracks whether we're currently in the "bus not Configured" state, so
/// disconnect noise only emits on the true → false transition.
///
/// Initialized to `true` because at boot the bus genuinely is disconnected
/// (host hasn't enumerated yet) — the first transition out of that state
/// into Configured is the interesting one, not arrival at it. `Suspend`
/// (which the bus enters after ~3 ms of no SOFs — i.e. constantly when
/// the host isn't actively moving data) also reports as not-Configured
/// from the driver, so without this gate every quiet moment spams the log.
static USB_DISCONNECTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);

/// Encode and write a `TunnelError` frame straight to EP IN. Used both for
/// no-host-forwarding NACKs and for malformed-request rejects. Wraps the
/// frame with CRC16 + optional 1-byte pad so the host's `unwrap_frame`
/// accepts it — same wire format as `deliver_reply` and the fob channel.
fn write_tunnel_error(
    ep_in: &UsbEndpoint,
    seq: u16,
    code: rcard_usb_proto::messages::TunnelErrorCode,
) {
    let msg = rcard_usb_proto::messages::TunnelError { code };
    let mut frame = [0u8; 16];
    let Some(n) = rcard_usb_proto::simple::encode_simple(&msg, &mut frame, seq) else {
        return;
    };
    // Tunnel error frames are tiny (~7–8 bytes), so wrapped wire never
    // exceeds 64 — always one short packet, no chunking needed.
    let crc = crc16(&frame[..n]);
    let mut wire = [0u8; 19]; // 16 + 2 CRC + 1 pad
    wire[..n].copy_from_slice(&frame[..n]);
    wire[n] = (crc >> 8) as u8;
    wire[n + 1] = crc as u8;
    let mut total = n + 2;
    if total % 64 == 0 {
        wire[total] = 0;
        total += 1;
    }
    LAST_SENT.with(|ls| {
        *ls = LastSent::Error {
            buf: wire,
            len: total,
        };
    });
    let _ = write_usb(ep_in, &wire[..total]);
}

#[ipc::notification_handler(usb_event)]
fn handle_usb_event(_sender: u16, _code: u32) {
    let Some(ep_out) = EP_OUT.get() else { return };

    // If host_proxy hasn't consumed the previous request yet, or a
    // retransmit is in flight, don't drain further USB bytes.
    let busy = ACCUM
        .with(|a| matches!(a.state, UsbTunnelState::Staged | UsbTunnelState::Retransmitting))
        .unwrap_or(false);
    if busy {
        return;
    }

    // Drain packets into the tunnel buffer. A short packet (< 64 bytes) marks
    // end-of-transfer; each bulk transfer carries exactly one IPC frame
    // + trailing CRC16 (+ optional 1-byte pad). When end-of-transfer
    // arrives, we validate CRC and dispatch or NACK.
    let mut usb_buf = [0u8; 64];
    loop {
        match ep_out.read(&mut usb_buf) {
            Ok(Ok(n)) if n > 0 => {
                USB_DISCONNECTED.store(false, core::sync::atomic::Ordering::Relaxed);
                let n = n as usize;
                let is_short = n < 64;
                ACCUM
                    .with(|a| {
                        if a.state == UsbTunnelState::Overflowed {
                            return;
                        }
                        if a.state == UsbTunnelState::Idle {
                            if !tunnel().try_acquire_or_wipe(SELF_TID, refresh_tid) {
                                a.state = UsbTunnelState::Overflowed;
                                return;
                            }
                            a.state = UsbTunnelState::Accumulating;
                        }
                        let tun_data = unsafe { tunnel().data_mut() };
                        if a.len + n > tun_data.len() {
                            a.state = UsbTunnelState::Overflowed;
                            a.len = 0;
                            return;
                        }
                        tun_data[a.len..a.len + n].copy_from_slice(&usb_buf[..n]);
                        a.len += n;
                    })
                    .log_unwrap();

                if is_short {
                    // End of transfer — either validate and dispatch, or NACK.
                    let staged = process_accumulated_frame();
                    if staged {
                        let _ = Reactor::push(
                            notifications::GROUP_ID_HOST_REQUEST,
                            0,
                            20,
                            OverflowStrategy::Reject,
                        );
                    }
                    // A synchronous host won't pipeline — return so the
                    // next frame's first packets don't accidentally append
                    // onto a freshly-cleared buffer mid-stream.
                    return;
                }
            }
            Ok(Err(UsbError::Disconnected)) => {
                // Only act on the transition from connected → disconnected.
                // `UsbError::Disconnected` is also returned for transient
                // non-Configured states (Suspend, Default during enumeration),
                // so firing every time would spam the log and repeatedly wipe
                // the frame buffer for no reason.
                let was_connected =
                    !USB_DISCONNECTED.swap(true, core::sync::atomic::Ordering::Relaxed);
                if was_connected {
                    warn!("USB disconnected");
                    let _ = Reactor::push(
                        notifications::GROUP_ID_HOST_REQUEST,
                        0xFFFF_FFFF,
                        20,
                        OverflowStrategy::Reject,
                    );
                    ACCUM
                        .with(|a| {
                            if a.state != UsbTunnelState::Idle {
                                tunnel().release();
                            }
                            a.state = UsbTunnelState::Idle;
                            a.len = 0;
                        })
                        .log_unwrap();
                }
                return;
            }
            _ => break,
        }
    }
}

/// Validate the CRC over the accumulated transfer and stage the frame.
/// Returns `true` iff a request was staged for `host_proxy`.
fn process_accumulated_frame() -> bool {
    ACCUM
        .with(|a| {
            let n = a.len;
            let tun = tunnel();
            let tun_data = unsafe { tun.data_ref() };

            if a.state == UsbTunnelState::Overflowed {
                a.state = UsbTunnelState::Idle;
                a.len = 0;
                tun.release();
                info!("USB RX transfer overflowed, NACKing");
                send_request_corrupted();
                return false;
            }

            const HEADER_SIZE: usize = rcard_usb_proto::header::HEADER_SIZE;
            if n < HEADER_SIZE + 2 {
                a.state = UsbTunnelState::Idle;
                a.len = 0;
                tun.release();
                info!("USB RX transfer too short ({} bytes), NACKing", n);
                send_request_corrupted();
                return false;
            }

            let header = match rcard_usb_proto::FrameHeader::decode(&tun_data[..n]) {
                Ok(h) => h,
                Err(_) => {
                    a.state = UsbTunnelState::Idle;
                    a.len = 0;
                    tun.release();
                    info!("USB RX bad frame header, NACKing");
                    send_request_corrupted();
                    return false;
                }
            };

            let frame_size = HEADER_SIZE + header.length as usize;
            if frame_size + 2 > n {
                a.state = UsbTunnelState::Idle;
                a.len = 0;
                tun.release();
                info!(
                    "USB RX transfer short for declared length ({} < {} + 2), NACKing",
                    n, frame_size
                );
                send_request_corrupted();
                return false;
            }

            let expected = u16::from_be_bytes([tun_data[frame_size], tun_data[frame_size + 1]]);
            let actual = crc16(&tun_data[..frame_size]);
            if expected != actual {
                a.state = UsbTunnelState::Idle;
                a.len = 0;
                tun.release();
                info!("USB RX CRC mismatch, NACKing");
                send_request_corrupted();
                return false;
            }

            let frame = rcard_usb_proto::RawFrame {
                header,
                payload: &tun_data[HEADER_SIZE..frame_size],
            };

            if let Some(tunnel_err) = frame.parse_simple::<rcard_usb_proto::messages::TunnelError>()
            {
                if tunnel_err.code == rcard_usb_proto::messages::TunnelErrorCode::ReplyCorrupted {
                    a.state = UsbTunnelState::Retransmitting;
                    a.len = 0;
                    tun.release();
                    info!("host requested reply retransmit");
                    let retransmitted = LAST_SENT
                        .with(|ls| match ls {
                            LastSent::IpcReply => {
                                // host_proxy will call deliver_reply(),
                                // which clears Retransmitting → Idle.
                                let _ = Reactor::push(
                                    notifications::GROUP_ID_HOST_REQUEST,
                                    1,
                                    20,
                                    OverflowStrategy::Reject,
                                );
                                true
                            }
                            LastSent::Error { buf, len } => {
                                if let Some(ep_in) = EP_IN.get() {
                                    let _ = write_usb(ep_in, &buf[..*len]);
                                }
                                // Done inline — clear state now.
                                a.state = UsbTunnelState::Idle;
                                true
                            }
                            LastSent::None => false,
                        })
                        .unwrap_or(false);
                    if !retransmitted {
                        warn!("retransmit requested but nothing cached");
                        a.state = UsbTunnelState::Idle;
                    }
                    return false;
                } else {
                    a.state = UsbTunnelState::Idle;
                    a.len = 0;
                    tun.release();
                    warn!("host reported tunnel error: {}", tunnel_err.code);
                    return false;
                }
            }

            if frame.as_ipc_request().is_none() {
                a.state = UsbTunnelState::Idle;
                a.len = 0;
                tun.release();
                warn!("Unexpected frame type on host channel");
                return false;
            }

            if !HOST_FORWARDING_AVAILABLE {
                a.state = UsbTunnelState::Idle;
                a.len = 0;
                tun.release();
                if let Some(ep_in) = EP_IN.get() {
                    write_tunnel_error(
                        ep_in,
                        header.seq,
                        rcard_usb_proto::messages::TunnelErrorCode::NoHostForwarding,
                    );
                }
                return false;
            }

            unsafe { tun.set_len(frame_size as u32) };
            tun.transfer(HOST_PROXY_TID);
            a.state = UsbTunnelState::Staged;
            true
        })
        .unwrap_or(false)
}

fn send_request_corrupted() {
    if let Some(ep_in) = EP_IN.get() {
        write_tunnel_error(
            ep_in,
            0xFFFF,
            rcard_usb_proto::messages::TunnelErrorCode::RequestCorrupted,
        );
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
            interface_group: 0,
        },
    )
    .log_expect("EP OUT IPC failed")
    .log_expect("EP OUT open failed");

    let ep_in = UsbEndpoint::open(
        handles.ep_in,
        EndpointConfig {
            number: 5,
            direction: Direction::In,
            transfer_type: TransferType::Bulk,
            max_packet_size: 64,
            interval: 0,
            interface_group: 0,
        },
    )
    .log_expect("EP IN IPC failed")
    .log_expect("EP IN open failed");

    EP_OUT.set(ep_out).ok();
    EP_IN.set(ep_in).ok();

    info!("Host tunnel ready, entering notification loop");

    ipc::server! {
        HostTransport: HostTransportImpl,
        @notifications(Reactor) => handle_usb_event,
    }
}
