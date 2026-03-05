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
pub enum Error<E = ()> {
    /// The server task died and could not be reconnected.
    ServerDied,
    /// The server's resource arena is full; could not allocate.
    ArenaFull,
    /// The handle is stale or invalid — the resource was already freed.
    InvalidHandle,
    /// Server died; reconnect attempt ran the constructor, which returned `Err(e)`.
    ReconstructionFailed(E),
    /// Server died; reconnect attempt ran the constructor, which returned `None`.
    ReconstructionReturnedNone,
}

impl<E: hubpack::SerializedSize> hubpack::SerializedSize for Error<E> {
    // 1 byte hubpack enum discriminant + largest variant payload (ConstructorFailed(E)).
    const MAX_SIZE: usize = 1 + E::MAX_SIZE;
}

mod arena;
mod handle;
mod server;

pub use arena::Arena;
pub use handle::{IMPLICIT_DESTROY_METHOD, Meta, RawHandle, opcode, split_opcode};
pub use ipc_macros::resource;
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
