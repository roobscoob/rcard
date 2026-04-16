//! USB IPC transport — thin wrapper around [`ipc_protocol::IpcProtocol`]
//! that sends frames via USB bulk-OUT and fulfills replies from the
//! host-driven bulk-IN reader.

use std::sync::Arc;

use nusb::Endpoint;
use nusb::transfer::{Buffer, Bulk, Out};
use rcard_usb_proto::IpcRequest;
use tokio::sync::Mutex;

// Re-export the protocol types so existing consumers don't break.
pub use ipc_protocol::{IpcCallResult, IpcError, IpcProtocol, ResolvedResponse};

/// USB-specific IPC capability. Wraps an `IpcProtocol` with exclusive
/// ownership of the bulk-OUT endpoint for sending.
pub struct Ipc {
    protocol: Arc<IpcProtocol>,
    /// Mutex-guarded because `Endpoint::submit` / `next_complete`
    /// require `&mut self`, and multiple IPC callers may race.
    out_endpoint: Mutex<Endpoint<Bulk, Out>>,
}

impl Ipc {
    pub(crate) fn new(out_endpoint: Endpoint<Bulk, Out>) -> Self {
        Ipc {
            protocol: Arc::new(IpcProtocol::new()),
            out_endpoint: Mutex::new(out_endpoint),
        }
    }

    /// The underlying protocol handler — shared with the reader task
    /// so it can call `resolve()` when reply frames arrive.
    pub fn protocol(&self) -> &Arc<IpcProtocol> {
        &self.protocol
    }

    /// Send an IPC request and wait for the response.
    pub async fn call(&self, req: &IpcRequest<'_>) -> Result<IpcCallResult, IpcError> {
        self.protocol
            .call(req, |frame_bytes| async move {
                let mut buf = Buffer::new(frame_bytes.len());
                buf.extend_from_slice(&frame_bytes);

                let mut ep = self.out_endpoint.lock().await;
                ep.submit(buf);
                let completion = ep.next_complete().await;
                completion.status.map_err(|e| e.to_string())
            })
            .await
    }

    /// Called by the host-driven reader task to fulfill a pending request.
    pub(crate) async fn resolve(&self, seq: u16, response: ResolvedResponse) {
        self.protocol.resolve(seq, response).await;
    }

    /// Mark this transport dead. Callers still awaiting replies see
    /// `IpcError::Disconnected` (via the dropped oneshot); subsequent
    /// `call()` invocations fail fast with `IpcError::TransportClosed`.
    pub async fn poison(&self) {
        self.protocol.poison().await;
    }
}
