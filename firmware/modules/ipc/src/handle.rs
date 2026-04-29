/// Opaque handle to a resource instance — just a u64 key.
/// The arena maps this to an internal (slot, generation) pair.
#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    zerocopy::IntoBytes,
    zerocopy::FromBytes,
    zerocopy::Immutable,
    zerocopy::KnownLayout,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(transparent)]
pub struct RawHandle(pub u64);

impl RawHandle {
    pub const SIZE: usize = 8;
}

/// Combines a resource kind and method id into a u16 opcode.
#[inline]
pub const fn opcode(kind: u8, method: u8) -> u16 {
    ((kind as u16) << 8) | method as u16
}

/// Splits a u16 opcode into (kind, method).
#[inline]
pub const fn split_opcode(op: u16) -> (u8, u8) {
    ((op >> 8) as u8, op as u8)
}

/// Reserved method ID used by the implicit Drop destructor on client handles.
pub const IMPLICIT_DESTROY_METHOD: u8 = 0xFF;

/// Reserved method ID for cloning a refcounted handle to another task.
pub const CLONE_METHOD: u8 = 0xFD;

/// Reserved method ID for 2PC prepare_transfer.
pub const PREPARE_TRANSFER_METHOD: u8 = 0xFC;

/// Reserved method ID for 2PC cancel_transfer.
pub const CANCEL_TRANSFER_METHOD: u8 = 0xFB;

/// Reserved method ID for 2PC acquire (complete transfer).
pub const ACQUIRE_METHOD: u8 = 0xFA;

/// Reserved method ID for 2PC try_drop (cleanup after failed transfer).
pub const TRY_DROP_METHOD: u8 = 0xF9;

/// Reserved method ID for notify_dead (panic handler cleanup).
/// The server runs `cleanup_client` for the sender across all dispatchers.
pub const NOTIFY_DEAD_METHOD: u8 = 0xF8;

/// Reserved method ID for revoking all handles owned by the sender.
/// Used by host_proxy to clean up orphaned handles when a transport dies.
pub const REVOKE_ALL_METHOD: u8 = 0xF7;

/// Metadata about the incoming message, passed to handler methods.
///
/// Firmware-only: `Meta` carries a kernel `TaskId` and is produced by
/// the server dispatcher. Host builds elide it since they never
/// run a dispatcher.
#[cfg(target_os = "none")]
#[derive(Copy, Clone, Debug)]
pub struct Meta {
    pub sender: crate::kern::TaskId,
    pub lease_count: u8,
}
