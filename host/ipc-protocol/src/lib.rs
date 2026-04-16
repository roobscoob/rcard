//! Sans-io IPC request/reply matching protocol.
//!
//! This crate owns the sequence-number tracking and pending-request
//! map. It knows nothing about USB or USART2 — it just encodes
//! requests into bytes (via `rcard_usb_proto::FrameWriter`) and
//! resolves incoming reply frames against pending oneshot channels.
//!
//! Transports call:
//! - [`IpcProtocol::prepare_call`] to register a pending request,
//!   get the encoded frame bytes and a result receiver.
//! - [`IpcProtocol::resolve`] when a reply frame arrives, to fulfill
//!   the matching pending request.
//!
//! The transport is responsible for actually sending the bytes and
//! feeding received frames into `resolve`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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
    /// The response channel was dropped (reader task died).
    Disconnected,
    /// Failed to encode the request frame.
    Encode,
    /// The device responded with an unrecognized frame type.
    UnexpectedFrame,
    /// Transport-specific send error.
    Transport(String),
    /// Transport has been shut down — no new calls will be dispatched.
    TransportClosed,
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "IPC call timed out"),
            Self::TunnelError(code) => write!(f, "tunnel error: {code:?}"),
            Self::Disconnected => write!(f, "IPC response channel disconnected"),
            Self::Encode => write!(f, "failed to encode IPC request"),
            Self::UnexpectedFrame => write!(f, "device responded with unrecognized frame"),
            Self::Transport(e) => write!(f, "transport error: {e}"),
            Self::TransportClosed => write!(f, "transport closed"),
        }
    }
}

impl std::error::Error for IpcError {}

/// Resolved IPC response — produced by the transport's reader when a
/// reply frame arrives.
pub enum ResolvedResponse {
    Reply(IpcCallResult),
    TunnelError(TunnelErrorCode),
    UnexpectedFrame,
}

/// A prepared IPC call: encoded frame bytes + a result receiver.
/// The transport sends the bytes and the caller awaits the receiver.
pub struct PreparedCall {
    /// The encoded `rcard_usb_proto` IPC request frame, ready to send.
    /// For USB: write directly to bulk-OUT.
    /// For USART2: wrap in TYPE_IPC_REQUEST + COBS before writing.
    pub frame_bytes: Vec<u8>,
    /// Sequence number for this call (matches the frame header).
    pub seq: u16,
    /// Receiver that will yield the result when the reply arrives.
    pub result_rx: oneshot::Receiver<ResolvedResponse>,
}

/// Sans-io IPC protocol handler. Owns the frame encoder (sequence
/// numbers) and the pending-request map.
///
/// Thread-safe via internal `Mutex`es — can be shared between a
/// reader task (calling `resolve`) and callers (calling `prepare_call`
/// / `call`).
pub struct IpcProtocol {
    writer: Mutex<FrameWriter>,
    pending: Mutex<HashMap<u16, oneshot::Sender<ResolvedResponse>>>,
    /// Set once the transport is gone. New calls return `TransportClosed`
    /// immediately instead of registering a pending request that will
    /// never be resolved.
    poisoned: AtomicBool,
}

impl IpcProtocol {
    pub fn new() -> Self {
        IpcProtocol {
            writer: Mutex::new(FrameWriter::new()),
            pending: Mutex::new(HashMap::new()),
            poisoned: AtomicBool::new(false),
        }
    }

    /// Mark this protocol instance dead. Drains the pending map (each
    /// waiter receives `IpcError::Disconnected` via the dropped oneshot)
    /// and causes subsequent `prepare_call` / `call` invocations to fail
    /// fast with `IpcError::TransportClosed`.
    ///
    /// Scope is this instance only — other transports owning their own
    /// `IpcProtocol` are unaffected.
    pub async fn poison(&self) {
        self.poisoned.store(true, Ordering::SeqCst);
        self.pending.lock().await.clear();
    }

    /// Encode a request and register it as pending. Returns the raw
    /// frame bytes and a receiver for the result.
    ///
    /// The caller (transport) is responsible for actually sending the
    /// bytes over the wire.
    pub async fn prepare_call(
        &self,
        req: &IpcRequest<'_>,
    ) -> Result<PreparedCall, IpcError> {
        if self.poisoned.load(Ordering::SeqCst) {
            return Err(IpcError::TransportClosed);
        }
        let (tx, rx) = oneshot::channel();

        let mut buf = [0u8; 4096];
        let (seq, n) = {
            let mut writer = self.writer.lock().await;
            let seq = writer.current_seq();
            let n = writer
                .write_ipc_request(req, &mut buf)
                .ok_or(IpcError::Encode)?;
            (seq, n)
        };

        // Register pending BEFORE sending to avoid race.
        self.pending.lock().await.insert(seq, tx);

        Ok(PreparedCall {
            frame_bytes: buf[..n].to_vec(),
            seq,
            result_rx: rx,
        })
    }

    /// High-level call: prepare, let the caller send, then await the
    /// reply with a timeout.
    ///
    /// `send_fn` is called with the encoded frame bytes. It should
    /// return `Ok(())` if the bytes were sent successfully, or an
    /// error string if sending failed.
    pub async fn call<F, Fut>(
        &self,
        req: &IpcRequest<'_>,
        send_fn: F,
    ) -> Result<IpcCallResult, IpcError>
    where
        F: FnOnce(Vec<u8>) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let prepared = self.prepare_call(req).await?;

        // Send via the transport.
        send_fn(prepared.frame_bytes)
            .await
            .map_err(IpcError::Transport)?;

        // Wait for response with timeout.
        let response = match tokio::time::timeout(CALL_TIMEOUT, prepared.result_rx).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(_)) => return Err(IpcError::Disconnected),
            Err(_) => {
                self.pending.lock().await.remove(&prepared.seq);
                return Err(IpcError::Timeout);
            }
        };

        match response {
            ResolvedResponse::Reply(result) => Ok(result),
            ResolvedResponse::TunnelError(code) => Err(IpcError::TunnelError(code)),
            ResolvedResponse::UnexpectedFrame => Err(IpcError::UnexpectedFrame),
        }
    }

    /// Called by the transport's reader task when a reply frame arrives.
    /// If the sequence number matches a pending request, the oneshot is
    /// fulfilled. If not (e.g. unsolicited control events), returns
    /// `false` so the transport can dispatch the frame elsewhere.
    pub async fn resolve(&self, seq: u16, response: ResolvedResponse) -> bool {
        if let Some(tx) = self.pending.lock().await.remove(&seq) {
            let _ = tx.send(response);
            true
        } else {
            false
        }
    }
}
