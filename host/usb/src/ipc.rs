use std::collections::HashMap;
use std::time::Duration;

use nusb::transfer::TransferError;
use rcard_usb_proto::messages::tunnel_error::TunnelErrorCode;
use rcard_usb_proto::{FrameWriter, IpcRequest};
use tokio::sync::{Mutex, oneshot};

/// Default timeout for IPC calls.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Result of a successful IPC call.
#[derive(Clone, Debug)]
pub struct IpcCallResult {
    /// Kernel response code.
    pub rc: u32,
    /// Serialized return value.
    pub return_value: Vec<u8>,
    /// Concatenated writeback data for Write/ReadWrite leases.
    pub lease_writeback: Vec<u8>,
}

/// Errors from an IPC call.
#[derive(Debug)]
pub enum IpcError {
    /// No response within the timeout.
    Timeout,
    /// Device returned a tunnel-level error (dispatch failed).
    TunnelError(TunnelErrorCode),
    /// USB transfer error.
    Usb(TransferError),
    /// The response channel was dropped (reader task died).
    Disconnected,
    /// Failed to encode the request frame.
    Encode,
    /// The fob responded with an unrecognized frame type.
    UnexpectedFrame,
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "IPC call timed out"),
            Self::TunnelError(code) => write!(f, "tunnel error: {code:?}"),
            Self::Usb(e) => write!(f, "USB transfer error: {e}"),
            Self::Disconnected => write!(f, "IPC response channel disconnected"),
            Self::Encode => write!(f, "failed to encode IPC request"),
            Self::UnexpectedFrame => write!(f, "fob responded with unrecognized frame"),
        }
    }
}

impl std::error::Error for IpcError {}

/// Resolved IPC response.
pub(crate) enum ResolvedResponse {
    Reply(IpcCallResult),
    TunnelError(TunnelErrorCode),
    UnexpectedFrame,
}

/// IPC capability — send requests to firmware tasks over the host-driven
/// USB endpoint.
///
/// The host-driven reader task fulfills pending requests by calling
/// `resolve()`.
pub struct Ipc {
    writer: Mutex<FrameWriter>,
    pending: Mutex<HashMap<u16, oneshot::Sender<ResolvedResponse>>>,
    host_out: nusb::Interface,
    out_endpoint: u8,
}

impl Ipc {
    pub(crate) fn new(host_iface: nusb::Interface, out_endpoint: u8) -> Self {
        Ipc {
            writer: Mutex::new(FrameWriter::new()),
            pending: Mutex::new(HashMap::new()),
            host_out: host_iface,
            out_endpoint,
        }
    }

    /// Send an IPC request and wait for the response.
    pub async fn call(&self, req: &IpcRequest<'_>) -> Result<IpcCallResult, IpcError> {
        let (tx, rx) = oneshot::channel();

        // Encode and send.
        let mut buf = [0u8; 4096];
        let (seq, n) = {
            let mut writer = self.writer.lock().await;
            let seq = writer.current_seq();
            let n = writer
                .write_ipc_request(req, &mut buf)
                .ok_or(IpcError::Encode)?;
            (seq, n)
        };

        // Register pending before sending to avoid race.
        self.pending.lock().await.insert(seq, tx);

        // Send over USB.
        self.host_out
            .bulk_out(self.out_endpoint, buf[..n].to_vec())
            .await
            .into_result()
            .map_err(IpcError::Usb)?;

        // Wait for response with timeout.
        let response = match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(_)) => return Err(IpcError::Disconnected),
            Err(_) => {
                // Clean up the pending entry on timeout.
                self.pending.lock().await.remove(&seq);
                return Err(IpcError::Timeout);
            }
        };

        match response {
            ResolvedResponse::Reply(result) => Ok(result),
            ResolvedResponse::TunnelError(code) => Err(IpcError::TunnelError(code)),
            ResolvedResponse::UnexpectedFrame => Err(IpcError::UnexpectedFrame),
        }
    }

    /// Called by the host-driven reader task to fulfill a pending request.
    pub(crate) async fn resolve(&self, seq: u16, response: ResolvedResponse) {
        if let Some(tx) = self.pending.lock().await.remove(&seq) {
            let _ = tx.send(response);
        }
    }
}
