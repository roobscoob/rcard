#![cfg_attr(target_os = "none", no_std)]

/// Maximum size of a Hubris message or reply buffer, in bytes.
pub const HUBRIS_MESSAGE_SIZE_LIMIT: usize = 256;

// Kernel-adjacent modules — firmware only. On host targets (used by
// the schema-dump tool and host/ipc-runtime) these are elided entirely
// so the crate compiles without userlib/generated.
#[cfg(target_os = "none")]
pub mod kern;
#[cfg(target_os = "none")]
pub mod dispatch;
#[cfg(target_os = "none")]
pub mod call;

pub mod wire;

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
#[derive(
    Debug, Copy, Clone,
    zerocopy::TryFromBytes, zerocopy::IntoBytes,
    zerocopy::KnownLayout, zerocopy::Immutable,
)]
#[repr(u8)]
pub enum Error {
    /// The server task died.
    ServerDied = 0,
    /// The server's resource arena is full; could not allocate.
    ArenaFull = 1,
    /// The handle was evicted by a higher-priority client, or is otherwise
    /// stale (freed, wrong owner, etc.).
    HandleLost = 2,
    /// A 2PC handle transfer failed (handle was evicted or released while
    /// pending, or the acquire was rejected).
    TransferFailed = 3,
}

/// `ResponseCode` sent by the server when a client message is malformed
/// (bad size, bad contents, or bad leases, or unknown kind byte).
/// The client's generated code panics on any non-SUCCESS response code.
#[cfg(target_os = "none")]
pub const MALFORMED_MESSAGE: kern::ResponseCode = kern::ResponseCode(1);

/// `ResponseCode` sent by the server when a client is not in the server's
/// runtime ACL (i.e. the task did not declare `uses-sysmodule` for this server).
/// The client's generated code panics on this response code.
#[cfg(target_os = "none")]
pub const ACCESS_VIOLATION: kern::ResponseCode = kern::ResponseCode(2);


#[cfg(target_os = "none")]
pub mod alloc_take;
#[cfg(target_os = "none")]
mod arena;
mod dyn_handle;
#[cfg(target_os = "none")]
pub mod errors;
mod handle;
#[cfg(target_os = "none")]
mod server;
#[cfg(target_os = "none")]
pub use generated::tasks::TASK_COUNT;

#[cfg(target_os = "none")]
pub use arena::{AllocError, Arena, CloneError, SharedArena};
pub use dyn_handle::DynHandle;
pub use handle::{
    ACQUIRE_METHOD, CANCEL_TRANSFER_METHOD, CLONE_METHOD, IMPLICIT_DESTROY_METHOD,
    NOTIFY_DEAD_METHOD, PREPARE_TRANSFER_METHOD, TRY_DROP_METHOD,
    RawHandle, opcode, split_opcode,
};
// `Meta` requires `kern::TaskId`, firmware-only.
#[cfg(target_os = "none")]
pub use handle::Meta;
pub use ipc_macros::{
    __check_uses, allocation, interface, notification_handler, resource, server,
};
#[cfg(target_os = "none")]
pub use call::call_send;
#[cfg(target_os = "none")]
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
///
/// Firmware-only: the `TaskId` and `CloneError` types live in kernel-adjacent
/// modules that are elided on host targets.
#[cfg(target_os = "none")]
pub trait Cloneable {
    fn clone_for(&self, new_owner: kern::TaskId) -> Result<DynHandle, CloneError>;
}

/// A `Sync` wrapper around a `TaskId` for use in statics. Safe because Hubris
/// tasks are single-threaded.
///
/// On first `get()`, asks the kernel for the peer's current generation via
/// `sys_refresh_task_id`. This ensures the first IPC call uses the correct
/// generation even if the peer has restarted since this task was compiled.
///
/// Firmware-only.
#[cfg(target_os = "none")]
pub struct StaticTaskId {
    id: core::cell::UnsafeCell<kern::TaskId>,
    refreshed: core::cell::Cell<bool>,
}

#[cfg(target_os = "none")]
unsafe impl Sync for StaticTaskId {}

#[cfg(target_os = "none")]
impl StaticTaskId {
    pub const fn new(id: kern::TaskId) -> Self {
        Self {
            id: core::cell::UnsafeCell::new(id),
            refreshed: core::cell::Cell::new(false),
        }
    }

    pub fn get(&self) -> kern::TaskId {
        if !self.refreshed.get() {
            let current = kern::sys_refresh_task_id(unsafe { *self.id.get() });
            unsafe { *self.id.get() = current; }
            self.refreshed.set(true);
            current
        } else {
            unsafe { *self.id.get() }
        }
    }

    pub fn set(&self, id: kern::TaskId) {
        unsafe {
            *self.id.get() = id;
        }
        self.refreshed.set(true);
    }
}

// ── Schema export types ───────────────────────────────────────────────
//
// These are referenced by the macro-emitted `__ipc_schema` module so
// the schema-dump tool can iterate every resource's methods, params,
// and their postcard-schema NamedType descriptors.

/// Descriptor for one IPC method in a resource trait.
#[cfg_attr(feature = "host", derive(serde::Serialize))]
pub struct MethodDesc {
    pub name: &'static str,
    pub id: u8,
    pub kind: &'static str,
    pub params: &'static [ParamDesc],
    pub lease_params: &'static [LeaseParamDesc],
    #[cfg_attr(feature = "host", serde(serialize_with = "serialize_opt_schema"))]
    pub return_schema: Option<&'static postcard_schema::schema::NamedType>,
}

/// A non-lease parameter's schema.
#[cfg_attr(feature = "host", derive(serde::Serialize))]
pub struct ParamDesc {
    pub name: &'static str,
    #[cfg_attr(feature = "host", serde(serialize_with = "serialize_schema"))]
    pub schema: &'static postcard_schema::schema::NamedType,
}

/// A lease parameter — no schema (it's just bytes), just the name and direction.
#[cfg_attr(feature = "host", derive(serde::Serialize))]
pub struct LeaseParamDesc {
    pub name: &'static str,
    pub mutable: bool,
}

/// Descriptor for a whole resource's method table.
#[cfg_attr(feature = "host", derive(serde::Serialize))]
pub struct ResourceDesc {
    pub name: &'static str,
    pub kind: u8,
    pub methods: &'static [MethodDesc],
}

// Host-only: serialize `&'static NamedType` by converting to
// `OwnedNamedType` (which derives Serialize) on the fly.
#[cfg(feature = "host")]
fn serialize_schema<S: serde::Serializer>(
    schema: &&'static postcard_schema::schema::NamedType,
    ser: S,
) -> Result<S::Ok, S::Error> {
    use postcard_schema::schema::owned::OwnedNamedType;
    let owned = OwnedNamedType::from(*schema);
    serde::Serialize::serialize(&owned, ser)
}

#[cfg(feature = "host")]
fn serialize_opt_schema<S: serde::Serializer>(
    schema: &Option<&'static postcard_schema::schema::NamedType>,
    ser: S,
) -> Result<S::Ok, S::Error> {
    use postcard_schema::schema::owned::OwnedNamedType;
    let owned = schema.map(OwnedNamedType::from);
    serde::Serialize::serialize(&owned, ser)
}

#[doc(hidden)]
pub use rcard_log as __rcard_log;

/// Re-exports for macro-generated code. Consumer crates should not rely
/// on these directly; they're an implementation detail.
#[doc(hidden)]
pub use postcard as __postcard;
#[doc(hidden)]
pub use postcard_schema as __postcard_schema;
#[doc(hidden)]
pub use serde as __serde;

/// Panic macro used by generated IPC client/server code.
/// By default uses `rcard_log::panic!` for structured logging visibility.
/// Enable the `bare-panics` feature on the **consumer crate** to use plain
/// `core::panic!` instead (needed for core sysmodules that can't use rcard_log).
///
/// The `cfg` check is evaluated at the macro expansion site, so each binary
/// crate's own `bare-panics` feature controls which path is taken — unaffected
/// by Cargo's workspace-wide feature unification.
#[macro_export]
#[doc(hidden)]
macro_rules! __ipc_panic {
    ($($tt:tt)*) => {{
        #[cfg(feature = "bare-panics")]
        { panic!($($tt)*) }
        #[cfg(not(feature = "bare-panics"))]
        { $crate::__rcard_log::panic!($($tt)*) }
    }}
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
                let _ = $crate::kern::sys_send(
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
