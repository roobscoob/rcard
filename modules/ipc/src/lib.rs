#![no_std]

/// Maximum size of a Hubris message or reply buffer, in bytes.
pub const HUBRIS_MESSAGE_SIZE_LIMIT: usize = 256;

/// IPC-layer error returned to all callers.
///
/// Layer breakdown:
/// - **Kernel panics** (`sys_reply_fault`): true protocol violations (bad opcode,
///   bad message bytes). The client never observes these — the kernel kills the
///   client task. Non-SUCCESS `rc` from `sys_send` indicates this; generated
///   client code panics on any non-SUCCESS `rc`.
/// - **`ipc::Error`** (this type): infrastructure-level failures that the
///   server communicates back as a serialized `Result<T, ipc::Error>` in the
///   reply body. The client deserializes and returns these.
/// - **Domain errors**: transparent to the IPC layer.
///   - Constructors: `Ok(Err(E))` inner result (outer `Ok` = IPC ok, inner `Err` = domain fail).
///   - Messages: `ReconstructionFailed(E)` / `ReconstructionReturnedNone` if the server
///     died and the automatic reconnect attempt itself failed.
///
/// `E` is the reconstruction domain-error type (the constructor's `E`). Defaults
/// to `()` for resources whose constructor is infallible.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Error {
    /// The server task died.
    ServerDied,
    /// The server's resource arena is full; could not allocate.
    ArenaFull,
    /// The handle was evicted by a higher-priority client, or is otherwise
    /// stale (freed, wrong owner, etc.).
    HandleLost,
}

impl hubpack::SerializedSize for Error {
    const MAX_SIZE: usize = 1;
}

/// `ResponseCode` sent by the server when a client message is malformed
/// (bad size, bad contents, or bad leases, or unknown kind byte).
/// The client's generated code panics on any non-SUCCESS response code.
pub const MALFORMED_MESSAGE: userlib::ResponseCode = userlib::ResponseCode(1);


pub mod alloc_take;
mod arena;
mod dyn_handle;
mod handle;
mod server;
mod task_count {
    include!(concat!(env!("OUT_DIR"), "/task_count.rs"));
}
pub use task_count::TASK_COUNT;

pub use arena::{Arena, CloneError, SharedArena};
pub use dyn_handle::DynHandle;
pub use handle::{
    CLONE_METHOD, IMPLICIT_DESTROY_METHOD, TRANSFER_METHOD, Meta, RawHandle, opcode, split_opcode,
};
pub use ipc_macros::{
    __check_uses, allocation, interface, notification_handler, resource, server,
};
pub use server::{ResourceDispatch, Server};

/// Trait used by generated dispatcher code to extract a resource from a
/// constructor's return value.  Implemented for bare `T` (infallible)
/// and `Result<T, E>` (fallible, error serialized back to caller).
pub trait ConstructorResult<T> {
    type Error;
    fn into_resource(self) -> Result<T, Self::Error>;
}

// Constructor returned `Self` directly — always succeeds.
impl<T> ConstructorResult<T> for T {
    type Error = core::convert::Infallible;
    fn into_resource(self) -> Result<T, Self::Error> {
        Ok(self)
    }
}

// Constructor returned `Result<Self, E>` — may fail with a domain error.
impl<T, E> ConstructorResult<T> for Result<T, E> {
    type Error = E;
    fn into_resource(self) -> Result<T, Self::Error> {
        self
    }
}

/// Implemented by every generated client handle. Enables `#[handle(move)]`.
///
/// Transfers ownership of this handle to `new_owner` by sending a `0xFE`
/// message to the handle's server. Consumes `self` and returns a `DynHandle`
/// that the recipient can use to interact with the resource.
pub trait Transferable {
    fn transfer_to(self, new_owner: userlib::TaskId) -> Result<DynHandle, Error>;
}

/// Implemented by generated client handles for refcounted resources.
/// Enables `#[handle(clone)]`.
///
/// Clones this handle for `new_owner` by sending a `0xFD` message to the
/// handle's server. Does NOT consume `self`. Returns a `DynHandle` with the
/// new handle key.
pub trait Cloneable {
    fn clone_for(&self, new_owner: userlib::TaskId) -> Result<DynHandle, Error>;
}

/// A `Sync` wrapper around a `TaskId` for use in statics. Safe because Hubris
/// tasks are single-threaded.
pub struct StaticTaskId(core::cell::UnsafeCell<userlib::TaskId>);

unsafe impl Sync for StaticTaskId {}

impl StaticTaskId {
    pub const fn new(id: userlib::TaskId) -> Self {
        Self(core::cell::UnsafeCell::new(id))
    }

    pub fn get(&self) -> userlib::TaskId {
        unsafe { *self.0.get() }
    }

    pub fn set(&self, id: userlib::TaskId) {
        unsafe {
            *self.0.get() = id;
        }
    }
}
