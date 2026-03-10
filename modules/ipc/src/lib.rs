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
    /// A 2PC handle transfer failed (handle was evicted or released while
    /// pending, or the acquire was rejected).
    TransferFailed,
}

impl hubpack::SerializedSize for Error {
    const MAX_SIZE: usize = 1;
}

/// `ResponseCode` sent by the server when a client message is malformed
/// (bad size, bad contents, or bad leases, or unknown kind byte).
/// The client's generated code panics on any non-SUCCESS response code.
pub const MALFORMED_MESSAGE: userlib::ResponseCode = userlib::ResponseCode(1);

/// `ResponseCode` sent by the server when a client is not in the server's
/// runtime ACL (i.e. the task did not declare `uses-sysmodule` for this server).
/// The client's generated code panics on this response code.
pub const ACCESS_VIOLATION: userlib::ResponseCode = userlib::ResponseCode(2);


pub mod alloc_take;
mod arena;
mod dyn_handle;
pub mod errors;
mod handle;
mod server;
mod task_count {
    include!(concat!(env!("OUT_DIR"), "/task_count.rs"));
}
pub use task_count::TASK_COUNT;

pub use arena::{Arena, CloneError, SharedArena};
pub use dyn_handle::DynHandle;
pub use handle::{
    ACQUIRE_METHOD, CANCEL_TRANSFER_METHOD, CLONE_METHOD, IMPLICIT_DESTROY_METHOD,
    NOTIFY_DEAD_METHOD, PREPARE_TRANSFER_METHOD, TRY_DROP_METHOD, Meta,
    RawHandle, opcode, split_opcode,
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
/// Provides the handle's identity (server, kind, raw handle) so the 2PC
/// transfer codegen can send `PREPARE_TRANSFER_METHOD` to the source server
/// and pass the `DynHandle` to the target.
pub trait Transferable {
    /// Extract the DynHandle info (server, kind, raw handle) without
    /// performing a transfer. Used by 2PC prepare codegen.
    fn transfer_info(&self) -> DynHandle;
}

/// Implemented by generated client handles for refcounted resources.
/// Enables `#[handle(clone)]`.
///
/// Clones this handle for `new_owner` by sending a `0xFD` message to the
/// handle's server. Does NOT consume `self`. Returns a `DynHandle` with the
/// new handle key.
pub trait Cloneable {
    fn clone_for(&self, new_owner: userlib::TaskId) -> Result<DynHandle, CloneError>;
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

/// Notify servers that this task is about to die (e.g. from a panic handler).
///
/// Sends `NOTIFY_DEAD_METHOD` to each listed server, which triggers
/// `cleanup_client` for this task across all of that server's resource
/// dispatchers. This eagerly frees handles and unfreezes any pending 2PC
/// transfers, rather than waiting for the generation-change detection on
/// the next message.
///
/// Contains a static `AtomicBool` re-entrancy guard. If the IPC sends
/// themselves trigger a panic, the re-entered invocation is a no-op.
/// Place this macro **after** panic logging so both the original and any
/// re-entrant panic are recorded:
///
/// **Limitation:** This macro only works with statically-bound handle types
/// (those created via `bind_*!` macros) because it calls `server_task_id()`.
/// Dynamic handles (`FooDyn` / `DynHandle`) carry their server ID at runtime
/// and cannot be passed here. In practice, notifying the server you have a
/// static binding for will also clean up any dyn handles on that same server.
/// Truly cross-server dyn handles (received via transfer from an unknown
/// server) are not covered — they fall back to generation-change detection.
///
/// ```ignore
/// #[panic_handler]
/// fn panic(info: &core::panic::PanicInfo) -> ! {
///     log_panic(info);                              // always runs — logs both panics
///     ipc::notify_dead!(GpioHandle, UartHandle);    // skipped on re-entry
///     loop {}
/// }
/// ```
#[macro_export]
macro_rules! notify_dead {
    () => {{}};
    ($($Server:ty),+ $(,)?) => {{
        use core::sync::atomic::{AtomicBool, Ordering};
        static REENTERED: AtomicBool = AtomicBool::new(false);
        if !REENTERED.swap(true, Ordering::Relaxed) {
            $(
                let _ = userlib::sys_send(
                    <$Server>::server_task_id(),
                    ipc::opcode(0, ipc::NOTIFY_DEAD_METHOD),
                    &[],
                    &mut [0u8; 0],
                    &mut [],
                );
            )+
        }
    }};
}
