//! RAII wrapper around an acquired IPC resource handle.
//!
//! `ResourceHandle` is what the host holds after a successful
//! constructor call (e.g. `Partition::acquire`). It carries everything
//! needed to (a) make follow-up `#[message]` calls on the server-side
//! arena slot and (b) fire the implicit-destroy opcode (kind, 0xFF) when
//! it's dropped, so the arena slot doesn't leak.

use std::sync::Arc;

use ipc_runtime::{
    self, CallError, EncodedCall, IpcValue, Registry, ReplyDecodeError, HandleExtractError,
};
use tokio::sync::{mpsc, oneshot};

use crate::bridge::Command;
use crate::state::DeviceId;

/// Reserved method ID for the implicit-destroy opcode (matches
/// `ipc::IMPLICIT_DESTROY_METHOD` in firmware/modules/ipc/src/handle.rs).
const IMPLICIT_DESTROY_METHOD: u8 = 0xFF;

/// An acquired IPC resource handle on a specific device. Drop sends the
/// implicit-destroy opcode over the bridge so the server releases its
/// arena slot.
pub struct ResourceHandle {
    device_id: DeviceId,
    resource_name: String,
    resource_kind: u8,
    task_id: u16,
    handle_id: u64,
    registry: Arc<Registry>,
    cmd_tx: mpsc::UnboundedSender<Command>,
}

impl ResourceHandle {
    /// Raw u64 handle identifier assigned by the server's arena. Mostly
    /// useful for log output.
    pub fn handle_id(&self) -> u64 {
        self.handle_id
    }

    /// Resource name (e.g. `"Partition"`). Useful for logs.
    pub fn resource_name(&self) -> &str {
        &self.resource_name
    }

    /// Call an instance method on this handle. `args` must be an
    /// `IpcValue::Struct` whose fields match the method's non-lease
    /// parameter names. Discards any writeback data from write-leases —
    /// use [`Self::call_with_writeback`] for methods like
    /// `Partition::read` that return bytes via a lease.
    pub async fn call(
        &self,
        method: &str,
        args: IpcValue,
    ) -> Result<IpcValue, IpcError> {
        self.call_with_writeback(method, args).await.map(|(v, _)| v)
    }

    /// Call an instance method and return both the decoded return value
    /// and the raw lease-writeback bytes. For methods whose reply comes
    /// back through a `&mut [u8]` lease parameter (e.g. `read`), the
    /// bytes the server wrote live in the writeback.
    pub async fn call_with_writeback(
        &self,
        method: &str,
        args: IpcValue,
    ) -> Result<(IpcValue, Vec<u8>), IpcError> {
        // Snapshot the method schema — we need it for decoding the reply.
        let schema = self
            .registry
            .method(&self.resource_name, method)
            .ok_or_else(|| {
                IpcError::Registry(format!("unknown method: {}::{method}", self.resource_name))
            })?
            .clone();

        let encoded = self
            .registry
            .encode_call_with_handle(
                &self.resource_name,
                method,
                self.handle_id,
                args,
            )
            .map_err(IpcError::Encode)?;

        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(Command::IpcCall {
                device_id: self.device_id,
                call: encoded,
                reply: reply_tx,
            })
            .map_err(|_| IpcError::Dispatch("bridge send failed".into()))?;

        let result = reply_rx
            .await
            .map_err(|_| IpcError::Dispatch("reply channel dropped".into()))?
            .map_err(IpcError::Dispatch)?;

        if result.rc != 0 {
            return Err(IpcError::KernelReject {
                rc: result.rc,
                bytes: result.return_value,
            });
        }

        let value = self
            .registry
            .decode_reply(&schema, &result.return_value)
            .map_err(IpcError::Decode)?;
        Ok((value, result.lease_writeback))
    }
}

impl Drop for ResourceHandle {
    fn drop(&mut self) {
        // Build a bare `(kind, 0xFF)` frame with args = postcard((RawHandle,)).
        // No method schema needed — the server treats this opcode
        // uniformly regardless of resource.
        let wire_args = match postcard::to_allocvec(&(self.handle_id,)) {
            Ok(v) => v,
            Err(_) => return, // infallible in practice; nothing we can do on drop
        };

        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(Command::IpcCall {
            device_id: self.device_id,
            call: EncodedCall {
                task_id: self.task_id,
                resource_kind: self.resource_kind,
                method_id: IMPLICIT_DESTROY_METHOD,
                wire_args,
                leases: Vec::new(),
                lease_data: Vec::new(),
            },
            reply: reply_tx,
        });
        // reply_rx dropped — we don't await the server's ack; bridge
        // already tolerates a dropped reply channel.
    }
}

/// Call a `StaticMessage` / `Constructor` method that does **not** take
/// a `&self` handle (e.g. `FlashLayout::get_layout`, `DeviceInfo::get_uid`).
/// Returns the decoded `IpcValue`.
pub async fn call_static(
    device_id: DeviceId,
    registry: &Arc<Registry>,
    cmd_tx: &mpsc::UnboundedSender<Command>,
    resource: &str,
    method: &str,
    args: IpcValue,
) -> Result<IpcValue, IpcError> {
    let schema = registry
        .method(resource, method)
        .ok_or_else(|| IpcError::Registry(format!("unknown method: {resource}::{method}")))?
        .clone();

    let encoded = registry
        .encode_call(resource, method, args)
        .map_err(IpcError::Encode)?;

    let (reply_tx, reply_rx) = oneshot::channel();
    cmd_tx
        .send(Command::IpcCall {
            device_id,
            call: encoded,
            reply: reply_tx,
        })
        .map_err(|_| IpcError::Dispatch("bridge send failed".into()))?;

    let result = reply_rx
        .await
        .map_err(|_| IpcError::Dispatch("reply channel dropped".into()))?
        .map_err(IpcError::Dispatch)?;

    if result.rc != 0 {
        return Err(IpcError::KernelReject {
            rc: result.rc,
            bytes: result.return_value,
        });
    }

    registry
        .decode_reply(&schema, &result.return_value)
        .map_err(IpcError::Decode)
}

/// Call a `Constructor` method (e.g. `Partition::acquire`) and, on
/// success, return a `ResourceHandle` whose Drop will release the slot
/// on the server. Unwraps the constructor's outer `Result` / `Option`
/// automatically.
pub async fn acquire(
    device_id: DeviceId,
    registry: &Arc<Registry>,
    cmd_tx: &mpsc::UnboundedSender<Command>,
    resource: &str,
    ctor_method: &str,
    args: IpcValue,
) -> Result<ResourceHandle, IpcError> {
    let reply_value =
        call_static(device_id, registry, cmd_tx, resource, ctor_method, args).await?;

    let handle_id = ipc_runtime::extract_handle(&reply_value).map_err(IpcError::HandleExtract)?;

    // Resource metadata we stash on the handle for follow-up calls
    // and the Drop path.
    let schema = registry
        .method(resource, ctor_method)
        .ok_or_else(|| IpcError::Registry(format!("unknown method: {resource}::{ctor_method}")))?;
    let resource_kind = schema.resource_kind;
    let task_id = registry.task_id_for(resource).unwrap_or(0);

    Ok(ResourceHandle {
        device_id,
        resource_name: resource.to_string(),
        resource_kind,
        task_id,
        handle_id,
        registry: registry.clone(),
        cmd_tx: cmd_tx.clone(),
    })
}

/// Unified error type surfaced by `AppState::acquire` / `call_static` /
/// `ResourceHandle::call`.
#[derive(Debug)]
pub enum IpcError {
    NoRegistry,
    DeviceMissing,
    Registry(String),
    Encode(CallError),
    Decode(ReplyDecodeError),
    HandleExtract(HandleExtractError),
    Dispatch(String),
    /// Server rejected with a kernel-level response code (e.g.
    /// `MALFORMED_MESSAGE=1`, `ACCESS_VIOLATION=2`). `bytes` is whatever
    /// the server passed as error data — typically empty, or a small
    /// diagnostic payload.
    KernelReject { rc: u32, bytes: Vec<u8> },
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRegistry => write!(f, "device has no IPC registry loaded"),
            Self::DeviceMissing => write!(f, "device not found"),
            Self::Registry(s) => write!(f, "registry: {s}"),
            Self::Encode(e) => write!(f, "encode: {e}"),
            Self::Decode(e) => write!(f, "decode: {e}"),
            Self::HandleExtract(e) => write!(f, "handle extract: {e}"),
            Self::Dispatch(s) => write!(f, "dispatch: {s}"),
            Self::KernelReject { rc, bytes } => {
                let label = match *rc {
                    1 => " (MALFORMED_MESSAGE)",
                    2 => " (ACCESS_VIOLATION)",
                    _ => "",
                };
                write!(f, "server rejected: rc={rc}{label} data={bytes:02x?}")
            }
        }
    }
}

impl std::error::Error for IpcError {}
