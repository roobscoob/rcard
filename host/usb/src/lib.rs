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

pub use ipc::{IpcCallResult, IpcError, IpcProtocol, UsbSender};
pub use hotplug::{FobEvent, watch_fobs};

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
/// endpoint's `max_packet_size` (64 on FS, 512 on HS); 4096 is safe for
/// both.
const IN_BUFFER_SIZE: usize = 4096;

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
    pub fn connect(
        id: AdapterId,
        serial: &str,
        sink: LogSink,
    ) -> Result<Self, ConnectError> {
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
            sender,
        ));

        let cancel = CancellationToken::new();

        let host_task = tokio::spawn(host_reader(
            host_in,
            protocol.clone(),
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
    sink: LogSink,
    cancel: CancellationToken,
) {
    use ipc::ResolvedResponse;
    use rcard_usb_proto::FrameReader;
    use rcard_usb_proto::messages::tunnel_error::TunnelError;

    let mut reader = FrameReader::<4096>::new();

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
            Ok(()) => completion.buffer,
            Err(e) => {
                sink.error(error::UsbError::Transfer(e));
                break;
            }
        };

        reader.push(&buf[..buf.len()]);

        loop {
            match reader.next_frame() {
                Ok(Some(frame)) => {
                    let size = frame.header.frame_size();

                    if let Some(response) = frame.as_ipc_response() {
                        let seq = response.seq;
                        let resolved = if let Some(reply) = response.as_reply() {
                            ResolvedResponse::Reply(IpcCallResult {
                                rc: reply.rc,
                                return_value: reply.return_value.to_vec(),
                                lease_writeback: reply.lease_writeback.to_vec(),
                            })
                        } else if let Some(err) = response.parse_simple::<TunnelError>() {
                            ResolvedResponse::TunnelError(err.code)
                        } else {
                            ResolvedResponse::UnexpectedFrame
                        };
                        protocol.resolve(seq, resolved).await;
                    }

                    reader.consume(size);
                }
                Ok(None) => break,
                Err(rcard_usb_proto::ReaderError::Oversized { declared_size }) => {
                    sink.error(error::UsbError::FrameOversize { declared_size });
                    reader.skip_frame(declared_size);
                }
                Err(e) => {
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
fn find_bulk_endpoint_address(
    dev: &nusb::Device,
    interface_num: u8,
    out: bool,
) -> Option<u8> {
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
