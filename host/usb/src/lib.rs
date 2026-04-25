mod crc;
pub mod error;
pub mod fob_reader;
pub mod hotplug;
pub mod ipc;

use std::any::{Any, TypeId};
use std::sync::Arc;

use device::adapter::{Adapter, AdapterId};
use device::device::LogSink;
use nusb::MaybeFuture;
use nusb::transfer::{Bulk, In, Out};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub use hotplug::{FobEvent, watch_fobs};
pub use ipc::{IpcCallResult, IpcError, IpcProtocol, UsbSender};

/// Priority of the USB transport relative to other `ipc_protocol::Ipc`
/// providers — higher wins. USB is preferred over USART2 because it's
/// faster and less likely to drop bytes under load.
pub const USB_IPC_PRIORITY: u8 = 10;

/// USB VID/PID for rcard fobs.
pub const USB_VID: u16 = 0x16D0;
pub const USB_PID: u16 = 0x14EF;

/// USB interface indices.
const HOST_DRIVEN_INTERFACE: u8 = 0;
const FOB_DRIVEN_INTERFACE: u8 = 1;

/// Size of each bulk IN buffer, in bytes. Must be a multiple of the
/// endpoint's `max_packet_size` (64 on FS, 512 on HS) — nusb / the OS
/// USB stack rejects non-aligned buffers, and the failure mode is that
/// `next_complete` immediately returns a transfer error which kills
/// the reader task.
///
/// Each bulk transfer carries exactly one IPC frame + 2-byte CRC16 +
/// optional 1-byte pad, so the buffer needs to fit the largest possible
/// frame (`MAX_DECODED_FRAME`) plus 3 framing bytes, rounded up to the
/// next 512-byte boundary (works on both FS and HS).
const IN_BUFFER_SIZE: usize = (rcard_usb_proto::MAX_DECODED_FRAME + 3 + 511) / 512 * 512;

/// Number of outstanding IN transfers to keep in-flight per reader.
/// More = higher throughput at the cost of memory; 2 is enough to keep
/// the pipeline full while we decode the previous buffer.
const IN_PIPELINE_DEPTH: usize = 2;

/// USB adapter — connects to a fob over USB.
///
/// Provides the `ipc_protocol::Ipc` capability on the host-driven channel,
/// and pushes fob-driven events (logs, etc.) into the device's LogSink.
pub struct Usb {
    id: AdapterId,
    serial: String,
    ipc: Arc<ipc_protocol::Ipc>,
    cancel: CancellationToken,
    host_task: Option<JoinHandle<()>>,
    fob_task: Option<JoinHandle<()>>,
}

impl Usb {
    /// Connect to the USB device whose string serial descriptor matches
    /// `serial` (case-insensitive). Errors if no such device is present
    /// or if its interfaces can't be claimed.
    pub fn connect(id: AdapterId, serial: &str, sink: LogSink) -> Result<Self, ConnectError> {
        let dev_info = nusb::list_devices()
            .wait()
            .map_err(ConnectError::Enumerate)?
            .find(|d| {
                d.vendor_id() == USB_VID
                    && d.product_id() == USB_PID
                    && d.serial_number()
                        .is_some_and(|s| s.eq_ignore_ascii_case(serial))
            })
            .ok_or_else(|| ConnectError::NotFound {
                serial: serial.to_string(),
            })?;

        let dev = dev_info.open().wait().map_err(ConnectError::Open)?;

        let host_iface = dev
            .claim_interface(HOST_DRIVEN_INTERFACE)
            .wait()
            .map_err(ConnectError::Claim)?;
        let fob_iface = dev
            .claim_interface(FOB_DRIVEN_INTERFACE)
            .wait()
            .map_err(ConnectError::Claim)?;

        let host_in_addr = find_bulk_endpoint_address(&dev, HOST_DRIVEN_INTERFACE, false)
            .ok_or(ConnectError::NoEndpoints)?;
        let host_out_addr = find_bulk_endpoint_address(&dev, HOST_DRIVEN_INTERFACE, true)
            .ok_or(ConnectError::NoEndpoints)?;
        let fob_in_addr = find_bulk_endpoint_address(&dev, FOB_DRIVEN_INTERFACE, false)
            .ok_or(ConnectError::NoEndpoints)?;

        let host_in: nusb::Endpoint<Bulk, In> = host_iface
            .endpoint(host_in_addr)
            .map_err(ConnectError::Claim)?;
        let host_out: nusb::Endpoint<Bulk, Out> = host_iface
            .endpoint(host_out_addr)
            .map_err(ConnectError::Claim)?;
        let fob_in: nusb::Endpoint<Bulk, In> = fob_iface
            .endpoint(fob_in_addr)
            .map_err(ConnectError::Claim)?;

        let protocol = Arc::new(IpcProtocol::new());
        let sender: Arc<dyn ipc_protocol::FrameSender> = Arc::new(UsbSender::new(host_out));
        let ipc = Arc::new(ipc_protocol::Ipc::new(
            "usb",
            USB_IPC_PRIORITY,
            protocol.clone(),
            sender.clone(),
        ));

        let cancel = CancellationToken::new();

        let host_task = tokio::spawn(host_reader(
            host_in,
            protocol.clone(),
            sender,
            sink.clone(),
            cancel.clone(),
        ));

        let fob_task = tokio::spawn(fob_reader::run(fob_in, sink, cancel.clone()));

        Ok(Usb {
            id,
            serial: serial.to_string(),
            ipc,
            cancel,
            host_task: Some(host_task),
            fob_task: Some(fob_task),
        })
    }

    /// The USB serial number this adapter was connected to. Stable
    /// identity for the fob (derived from the chip UID on the firmware
    /// side).
    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// Best-effort graceful shutdown. Poisons the IPC protocol so any
    /// pending callers see `TransportClosed`, cancels the reader tasks,
    /// and waits for them to exit.
    pub async fn shutdown(&mut self) {
        self.ipc.poison().await;
        self.cancel.cancel();

        for task in [self.host_task.take(), self.fob_task.take()] {
            if let Some(handle) = task {
                let _ = handle.await;
            }
        }
    }
}

impl Adapter for Usb {
    fn id(&self) -> AdapterId {
        self.id
    }

    fn display_name(&self) -> &str {
        "USB"
    }

    fn capabilities(&self) -> Vec<(TypeId, Arc<dyn Any + Send + Sync>)> {
        vec![(TypeId::of::<ipc_protocol::Ipc>(), self.ipc.clone())]
    }
}

impl Drop for Usb {
    fn drop(&mut self) {
        // Best-effort synchronous teardown. `shutdown()` is preferred
        // because it can `await` task completion; this path spawns a
        // detached task to poison IPC and cancels the reader tokens —
        // the next_complete futures are cancel-safe so tasks unwind
        // cleanly from the select!.
        let ipc = self.ipc.clone();
        tokio::spawn(async move {
            ipc.poison().await;
        });
        self.cancel.cancel();
    }
}

/// Errors from USB connection.
#[derive(Debug)]
pub enum ConnectError {
    Enumerate(nusb::Error),
    NotFound { serial: String },
    Open(nusb::Error),
    Claim(nusb::Error),
    NoEndpoints,
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Enumerate(e) => write!(f, "failed to enumerate USB devices: {e}"),
            Self::NotFound { serial } => {
                write!(f, "no rcard USB device with serial {serial}")
            }
            Self::Open(e) => write!(f, "failed to open USB device: {e}"),
            Self::Claim(e) => write!(f, "failed to claim USB interface: {e}"),
            Self::NoEndpoints => write!(f, "interface missing bulk endpoints"),
        }
    }
}

impl std::error::Error for ConnectError {}

// ── Host-driven reader task ──────────────────────────────────────────────

/// Read IPC responses from the host-driven bulk IN endpoint. Exits on
/// transfer error (device disconnect) or cancellation. On exit, poisons
/// the IPC protocol so any callers still awaiting a reply see a typed
/// error instead of a silent timeout.
async fn host_reader(
    mut endpoint: nusb::Endpoint<Bulk, In>,
    protocol: Arc<IpcProtocol>,
    sender: Arc<dyn ipc_protocol::FrameSender>,
    sink: LogSink,
    cancel: CancellationToken,
) {
    use ipc::ResolvedResponse;
    use rcard_usb_proto::FrameReader;
    use rcard_usb_proto::messages::tunnel_error::{TunnelError, TunnelErrorCode};

    let mut reader = FrameReader::<{ rcard_usb_proto::MAX_DECODED_FRAME }>::new();

    for _ in 0..IN_PIPELINE_DEPTH {
        endpoint.submit(nusb::transfer::Buffer::new(IN_BUFFER_SIZE));
    }

    loop {
        let completion = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            c = endpoint.next_complete() => c,
        };

        let buf = match completion.status {
            Ok(()) => {
                eprintln!(
                    "[usb-reader] IN completion: {} bytes",
                    completion.buffer.len(),
                );
                completion.buffer
            }
            Err(e) => {
                eprintln!("[usb-reader] IN transfer error: {e}");
                sink.error(error::UsbError::Transfer(e));
                break;
            }
        };

        // Each bulk IN completion is exactly one IPC frame + trailing
        // CRC16 (+ optional pad). Validate, strip, push. On mismatch,
        // discard the transfer — resync is automatic because the next
        // bulk transfer is, by USB definition, a fresh frame.
        match crc::unwrap_frame(&buf[..buf.len()]) {
            Ok(frame_bytes) => {
                eprintln!("[usb-reader] CRC OK, frame {} bytes", frame_bytes.len(),);
                reader.push(frame_bytes);
            }
            Err(e) => {
                eprintln!(
                    "[usb-reader] CRC FAILED ({e:?}), raw {} bytes — requesting retransmit",
                    buf.len(),
                );
                sink.error(error::UsbError::CrcMismatch);
                reader.reset();

                // Ask the device to retransmit the last reply.
                use rcard_usb_proto::messages::tunnel_error::{TunnelError, TunnelErrorCode};
                let msg = TunnelError {
                    code: TunnelErrorCode::ReplyCorrupted,
                };
                let mut frame_buf = [0u8; 16];
                if let Some(n) =
                    rcard_usb_proto::simple::encode_simple(&msg, &mut frame_buf, 0xFFFF)
                {
                    let wrapped = crc::wrap_frame(&frame_buf[..n]);
                    let _ = sender.send_frame(wrapped).await;
                    eprintln!("[usb-reader] sent ReplyCorrupted, awaiting retransmit");
                }

                // Resubmit the IN buffer before looping back — otherwise
                // the pipeline drains and next_complete panics.
                let mut buf = buf;
                buf.clear();
                endpoint.submit(buf);
                continue;
            }
        }

        loop {
            match reader.next_frame() {
                Ok(Some(frame)) => {
                    let size = frame.header.frame_size();

                    if let Some(response) = frame.as_ipc_response() {
                        let seq = response.seq;
                        let resolved = if let Some(reply) = response.as_reply() {
                            eprintln!(
                                "[usb-reader] IPC reply seq={seq} rc={} return={} wb={}",
                                reply.rc,
                                reply.return_value.len(),
                                reply.lease_writeback.len(),
                            );
                            ResolvedResponse::Reply(IpcCallResult {
                                rc: reply.rc,
                                return_value: reply.return_value.to_vec(),
                                lease_writeback: reply.lease_writeback.to_vec(),
                            })
                        } else if let Some(err) = response.parse_simple::<TunnelError>() {
                            eprintln!("[usb-reader] tunnel error seq={seq} code={:?}", err.code,);
                            ResolvedResponse::TunnelError(err.code)
                        } else {
                            eprintln!("[usb-reader] unexpected frame seq={seq}");
                            ResolvedResponse::UnexpectedFrame
                        };

                        // `RequestCorrupted` is unkeyed — the firmware
                        // couldn't recover the seq from a corrupt packet,
                        // so it sets seq=0xFFFF and expects us to retransmit
                        // every still-pending request.
                        if matches!(
                            resolved,
                            ResolvedResponse::TunnelError(TunnelErrorCode::RequestCorrupted)
                        ) {
                            protocol.retransmit_pending(&sender).await;
                        } else {
                            let matched = protocol.resolve(seq, resolved).await;
                            if !matched {
                                eprintln!("[usb-reader] seq={seq} had no pending caller!",);
                            }
                        }
                    } else {
                        eprintln!(
                            "[usb-reader] non-IPC frame, type={}, size={}",
                            frame.header.frame_type as u8, size,
                        );
                    }

                    reader.consume(size);
                }
                Ok(None) => break,
                Err(rcard_usb_proto::ReaderError::Oversized { declared_size }) => {
                    eprintln!("[usb-reader] oversized frame: {declared_size}");
                    sink.error(error::UsbError::FrameOversize { declared_size });
                    reader.skip_frame(declared_size);
                }
                Err(e) => {
                    eprintln!("[usb-reader] bad frame header: {e:?}");
                    sink.error(error::UsbError::BadFrameHeader(e));
                    reader.reset();
                    break;
                }
            }
        }

        // Resubmit the buffer for the next transfer. `requested_len` is
        // preserved across submits so we get another `IN_BUFFER_SIZE`.
        let mut buf = buf;
        buf.clear();
        endpoint.submit(buf);
    }

    // Transport gone — fail every pending IPC call with a typed error.
    protocol.poison().await;
}

// ── Endpoint discovery ───────────────────────────────────────────────────

/// Find the address of the bulk OUT (`out=true`) or bulk IN endpoint on
/// a given interface of the device's active configuration.
fn find_bulk_endpoint_address(dev: &nusb::Device, interface_num: u8, out: bool) -> Option<u8> {
    use nusb::descriptors::TransferType;
    use nusb::transfer::Direction;

    let active = dev.active_configuration().ok()?;
    for alt in active.interface_alt_settings() {
        if alt.interface_number() != interface_num {
            continue;
        }
        for ep in alt.endpoints() {
            if ep.transfer_type() != TransferType::Bulk {
                continue;
            }
            let is_out = ep.direction() == Direction::Out;
            if is_out == out {
                return Some(ep.address());
            }
        }
    }
    None
}

/// Enumerate currently-connected rcard fobs. Yields a `FobInfo` per
/// matching device. Synchronous — blocks via `MaybeFuture::wait`.
pub fn list_connected_fobs() -> Vec<FobInfo> {
    match nusb::list_devices().wait() {
        Ok(iter) => iter
            .filter(|d| d.vendor_id() == USB_VID && d.product_id() == USB_PID)
            .filter_map(|d| {
                d.serial_number().map(|s| FobInfo {
                    serial: s.to_string(),
                    id: d.id(),
                })
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Minimal identity info for a connected fob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FobInfo {
    pub serial: String,
    /// Stable USB device id — used to match disconnection events back
    /// to the fob without re-parsing the serial descriptor.
    pub id: nusb::DeviceId,
}
