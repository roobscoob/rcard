pub mod error;
pub mod fob_reader;
pub mod ipc;

use std::time::Duration;

/// Poll for a USB device with the rcard VID/PID whose serial number
/// matches `serial` (case-insensitive). Returns `true` on match, `false`
/// on timeout. Polls every 200ms.
pub async fn wait_for_serial(serial: &str, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(devices) = nusb::list_devices() {
            let found = devices.into_iter().any(|d| {
                d.vendor_id() == USB_VID
                    && d.product_id() == USB_PID
                    && d.serial_number()
                        .is_some_and(|s| s.eq_ignore_ascii_case(serial))
            });
            if found {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

use std::any::{Any, TypeId};
use std::sync::Arc;

use device::adapter::{Adapter, AdapterId};
use device::device::LogSink;
use tokio::task::JoinHandle;

pub use ipc::{Ipc, IpcCallResult, IpcError};

/// USB VID/PID for rcard fobs.
const USB_VID: u16 = 0x16D0;
const USB_PID: u16 = 0x14EF;

/// USB interface indices.
const HOST_DRIVEN_INTERFACE: u8 = 0;
const FOB_DRIVEN_INTERFACE: u8 = 1;

/// USB adapter — connects to a fob over USB.
///
/// Provides the `Ipc` capability on the host-driven channel, and pushes
/// fob-driven events (logs, etc.) into the device's LogSink.
pub struct Usb {
    id: AdapterId,
    ipc: Arc<Ipc>,
    _host_task: JoinHandle<()>,
    _fob_task: JoinHandle<()>,
}

impl Usb {
    /// Connect to the first matching USB device.
    pub fn connect(id: AdapterId, sink: LogSink) -> Result<Self, ConnectError> {
        // Find the device.
        let dev = nusb::list_devices()
            .map_err(ConnectError::Enumerate)?
            .find(|d| d.vendor_id() == USB_VID && d.product_id() == USB_PID)
            .ok_or(ConnectError::NotFound)?
            .open()
            .map_err(ConnectError::Open)?;

        // Claim both interfaces.
        let host_iface = dev
            .claim_interface(HOST_DRIVEN_INTERFACE)
            .map_err(ConnectError::Claim)?;
        let fob_iface = dev
            .claim_interface(FOB_DRIVEN_INTERFACE)
            .map_err(ConnectError::Claim)?;

        // Discover endpoint addresses from the interface descriptors.
        let (host_out, host_in) =
            find_bulk_endpoints(&dev, HOST_DRIVEN_INTERFACE).ok_or(ConnectError::NoEndpoints)?;
        let (_fob_out, fob_in) =
            find_bulk_endpoints(&dev, FOB_DRIVEN_INTERFACE).ok_or(ConnectError::NoEndpoints)?;

        // Create the Ipc capability (owns the host-driven OUT endpoint).
        let ipc = Arc::new(Ipc::new(host_iface.clone(), host_out));

        // Spawn the host-driven reader (fulfills IPC responses).
        let host_reader_ipc = ipc.clone();
        let host_sink = sink.clone();
        let host_task = tokio::spawn(host_reader(host_iface, host_in, host_reader_ipc, host_sink));

        // Spawn the fob-driven reader (dispatches events to LogSink).
        let fob_task = tokio::spawn(fob_reader::run(fob_iface, fob_in, sink));

        Ok(Usb {
            id,
            ipc,
            _host_task: host_task,
            _fob_task: fob_task,
        })
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
        vec![(TypeId::of::<Ipc>(), self.ipc.clone())]
    }
}

impl Drop for Usb {
    fn drop(&mut self) {
        self._host_task.abort();
        self._fob_task.abort();
    }
}

/// Errors from USB connection.
#[derive(Debug)]
pub enum ConnectError {
    Enumerate(std::io::Error),
    NotFound,
    Open(std::io::Error),
    Claim(std::io::Error),
    NoEndpoints,
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Enumerate(e) => write!(f, "failed to enumerate USB devices: {e}"),
            Self::NotFound => write!(f, "no rcard USB device found"),
            Self::Open(e) => write!(f, "failed to open USB device: {e}"),
            Self::Claim(e) => write!(f, "failed to claim USB interface: {e}"),
            Self::NoEndpoints => write!(f, "interface missing bulk endpoints"),
        }
    }
}

impl std::error::Error for ConnectError {}

// ── Host-driven reader task ──────────────────────────────────────────────

/// Read IPC responses from the host-driven bulk IN endpoint.
async fn host_reader(
    iface: nusb::Interface,
    in_endpoint: u8,
    ipc: Arc<Ipc>,
    sink: LogSink,
) {
    use ipc::ResolvedResponse;
    use rcard_usb_proto::messages::tunnel_error::TunnelError;
    use rcard_usb_proto::FrameReader;

    let mut reader = FrameReader::<4096>::new();

    loop {
        let data = match iface
            .bulk_in(in_endpoint, nusb::transfer::RequestBuffer::new(4096))
            .await
            .into_result()
        {
            Ok(data) => data,
            Err(e) => {
                sink.error(error::UsbError::Transfer(e));
                return;
            }
        };

        reader.push(&data);

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
                        ipc.resolve(seq, resolved).await;
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
    }
}

// ── Endpoint discovery ───────────────────────────────────────────────────

/// Find the bulk OUT and bulk IN endpoint addresses for a given interface.
fn find_bulk_endpoints(dev: &nusb::Device, interface_num: u8) -> Option<(u8, u8)> {
    let config = dev.active_configuration().ok()?;
    let iface = config
        .interfaces()
        .find(|i| i.interface_number() == interface_num)?;
    let alt = iface.alt_settings().next()?;

    let mut out = None;
    let mut r#in = None;

    for ep in alt.endpoints() {
        match ep.transfer_type() {
            nusb::transfer::EndpointType::Bulk => {
                if ep.direction() == nusb::transfer::Direction::Out {
                    out = Some(ep.address());
                } else {
                    r#in = Some(ep.address());
                }
            }
            _ => {}
        }
    }

    Some((out?, r#in?))
}
