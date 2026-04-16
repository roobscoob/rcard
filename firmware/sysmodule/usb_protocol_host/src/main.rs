#![no_std]
#![no_main]

use generated::notifications;
use generated::peers::PEERS;
use generated::slots::SLOTS;
use once_cell::{GlobalState, OnceCell};
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

const MAX_FRAME: usize = rcard_usb_proto::MAX_DECODED_FRAME;

// ---------------------------------------------------------------------------
// Pending request staging
//
// The host protocol is strictly synchronous request-response: the host
// will not send request N+1 until it has received the reply to N. So we
// only ever need one staged request at a time. `handle_usb_event` stashes
// the next frame here and signals `host_proxy` via the `host_request`
// notification group. `fetch_pending_request` copies it out, and
// `deliver_reply` clears the slot.
// ---------------------------------------------------------------------------

struct PendingRequest {
    buf: [u8; MAX_FRAME],
    len: usize,
    /// `true` between stage() and deliver_reply()'s clear.
    set: bool,
}

static PENDING: GlobalState<PendingRequest> = GlobalState::new(PendingRequest {
    buf: [0u8; MAX_FRAME],
    len: 0,
    set: false,
});

// ---------------------------------------------------------------------------
// Endpoints + frame reader
// ---------------------------------------------------------------------------

static EP_OUT: OnceCell<UsbEndpoint> = OnceCell::new();
static EP_IN: OnceCell<UsbEndpoint> = OnceCell::new();

/// Frame reader buffer. Persists across notification wakes so partial
/// frames received in one wake can be completed in a later wake.
static READER: GlobalState<rcard_usb_proto::FrameReader<4096>> =
    GlobalState::new(rcard_usb_proto::FrameReader::new());

// ---------------------------------------------------------------------------
// USB write helper
// ---------------------------------------------------------------------------

fn write_usb(ep_in: &UsbEndpoint, data: &[u8]) -> Result<(), HostTransportError> {
    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + 64).min(data.len());
        match ep_in.write(&data[offset..end]) {
            Ok(Ok(n)) => offset += n as usize,
            Ok(Err(UsbError::EndpointBusy)) => continue,
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
    ) -> Result<(), HostTransportError>
    {
        let Some(ep_in) = EP_IN.get() else {
            return Err(HostTransportError::WireWriteFailed);
        };

        // Stream the lease to USB in 64-byte chunks — matches the USB
        // bulk max packet size and avoids an 8 KB stack-local copy.
        let len = buf.len();
        let mut offset = 0;
        let mut result = Ok(());
        while offset < len {
            let mut chunk = [0u8; 64];
            let chunk_len = (len - offset).min(64);
            let _ = buf.read_range(offset, &mut chunk[..chunk_len]);
            if let Err(e) = write_usb(ep_in, &chunk[..chunk_len]) {
                result = Err(e);
                break;
            }
            offset += chunk_len;
        }

        // Always clear the pending slot even if USB write failed — the
        // dispatch is complete from host_proxy's perspective.
        PENDING
            .with(|p| {
                p.set = false;
                p.len = 0;
            })
            .log_unwrap();

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

/// Encode and write a `TunnelError` frame straight to EP IN. Used both for
/// no-host-forwarding NACKs and for malformed-request rejects.
fn write_tunnel_error(
    ep_in: &UsbEndpoint,
    seq: u16,
    code: rcard_usb_proto::messages::TunnelErrorCode,
) {
    let msg = rcard_usb_proto::messages::TunnelError { code };
    let mut buf = [0u8; 16];
    if let Some(n) = rcard_usb_proto::simple::encode_simple(&msg, &mut buf, seq) {
        let _ = write_usb(ep_in, &buf[..n]);
    }
}

#[ipc::notification_handler(usb_event)]
fn handle_usb_event(_sender: u16, _code: u32) {
    let Some(ep_out) = EP_OUT.get() else { return };

    // If host_proxy hasn't consumed the previous request yet, don't drain
    // further USB bytes into the reader. The protocol is synchronous so
    // the host won't send more anyway, and this keeps backpressure clean.
    let busy = PENDING.with(|p| p.set).unwrap_or(false);
    if busy {
        return;
    }

    // Drain everything currently buffered in EP OUT into the frame reader.
    let mut usb_buf = [0u8; 64];
    loop {
        match ep_out.read(&mut usb_buf) {
            Ok(Ok(n)) if n > 0 => {
                READER
                    .with(|r| r.push(&usb_buf[..n as usize]))
                    .log_unwrap();
            }
            Ok(Err(UsbError::Disconnected)) => {
                warn!("USB disconnected");
                READER.with(|r| r.reset()).log_unwrap();
                PENDING
                    .with(|p| {
                        p.set = false;
                        p.len = 0;
                    })
                    .log_unwrap();
                return;
            }
            _ => break,
        }
    }

    // Walk the reader for the next complete frame and either stage it
    // (if host forwarding is available) or NACK it (if not). Stop after
    // one stage — subsequent frames wait for host_proxy to drain the
    // current one. NACKs don't gate further frames since there's no
    // pending state to maintain.
    let mut staged = false;
    READER
        .with(|reader| loop {
            match reader.next_frame() {
                Ok(Some(frame)) => {
                    let size = frame.header.frame_size();
                    let seq = frame.header.seq;
                    if frame.as_ipc_request().is_some() {
                        if !HOST_FORWARDING_AVAILABLE {
                            // No host_proxy in this build — NACK the request
                            // immediately and keep draining.
                            if let Some(ep_in) = EP_IN.get() {
                                write_tunnel_error(
                                    ep_in,
                                    seq,
                                    rcard_usb_proto::messages::TunnelErrorCode::NoHostForwarding,
                                );
                            }
                            reader.consume(size);
                            continue;
                        }
                        // Copy the frame bytes into PENDING before consuming.
                        // host_proxy needs the wire-format frame (header
                        // + payload) so it can re-parse via FrameReader.
                        let payload_len = frame.payload.len();
                        let total = rcard_usb_proto::header::HEADER_SIZE + payload_len;
                        let ok = PENDING
                            .with(|p| {
                                if total > p.buf.len() {
                                    return false;
                                }
                                let mut header_buf =
                                    [0u8; rcard_usb_proto::header::HEADER_SIZE];
                                frame.header.encode(&mut header_buf);
                                p.buf[..rcard_usb_proto::header::HEADER_SIZE]
                                    .copy_from_slice(&header_buf);
                                p.buf[rcard_usb_proto::header::HEADER_SIZE..total]
                                    .copy_from_slice(frame.payload);
                                p.len = total;
                                p.set = true;
                                true
                            })
                            .unwrap_or(false);
                        reader.consume(size);
                        if ok {
                            staged = true;
                            return;
                        }
                    } else {
                        warn!("Unexpected frame type on host channel");
                        reader.consume(size);
                    }
                }
                Ok(None) => return,
                Err(rcard_usb_proto::ReaderError::Oversized { declared_size }) => {
                    warn!("Oversized frame, skipping");
                    reader.skip_frame(declared_size);
                }
                Err(rcard_usb_proto::ReaderError::Header(_)) => {
                    error!("Bad frame header, resetting");
                    reader.reset();
                    return;
                }
            }
        })
        .log_unwrap();

    if staged {
        // Wake host_proxy. Reject strategy: if it somehow can't be queued,
        // the next usb_event wake will re-push it.
        let _ = Reactor::push(
            notifications::GROUP_ID_HOST_REQUEST,
            0,
            20,
            OverflowStrategy::Reject,
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
