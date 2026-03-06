/// Opaque handle to a resource instance — just a u64 key.
/// The arena maps this to an internal (slot, generation) pair.
#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    zerocopy::IntoBytes,
    zerocopy::FromBytes,
    zerocopy::Immutable,
    zerocopy::KnownLayout,
)]
#[repr(transparent)]
pub struct RawHandle(pub u64);

impl RawHandle {
    pub const SIZE: usize = 8;
}

impl hubpack::SerializedSize for RawHandle {
    const MAX_SIZE: usize = <u64 as hubpack::SerializedSize>::MAX_SIZE;
}

/// Combines a resource kind and method id into a u16 opcode.
#[inline]
pub const fn opcode(kind: u8, method: u8) -> u16 {
    (kind as u16) << 8 | method as u16
}

/// Splits a u16 opcode into (kind, method).
#[inline]
pub const fn split_opcode(op: u16) -> (u8, u8) {
    ((op >> 8) as u8, op as u8)
}

/// Reserved method ID used by the implicit Drop destructor on client handles.
pub const IMPLICIT_DESTROY_METHOD: u8 = 0xFF;

/// Reserved method ID for transferring handle ownership to another task.
pub const TRANSFER_METHOD: u8 = 0xFE;

/// Reserved method ID for cloning a refcounted handle to another task.
pub const CLONE_METHOD: u8 = 0xFD;

/// Metadata about the incoming message, passed to handler methods.
#[derive(Copy, Clone, Debug)]
pub struct Meta {
    pub sender: userlib::TaskId,
    pub lease_count: u8,
}
