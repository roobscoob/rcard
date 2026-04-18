#![no_std]

// `ring` uses `storage_api::StorageDyn` and `Storage` is the `#[ipc::interface]`
// trait — both are firmware-only. The schema dumper compiles this crate on the
// host target just to read the schema-export const for `Partition`.
#[cfg(target_os = "none")]
pub mod ring;
pub use storage_api::{Geometry, StorageError};
#[cfg(target_os = "none")]
pub use storage_api::Storage;

include!(concat!(env!("OUT_DIR"), "/partition_names.rs"));

#[derive(
    Debug,
    Clone,
    Copy,
    rcard_log::Format,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum AcquireError {
    /// The partition is already acquired by another task.
    InUse = 0,
    /// No partition with this name exists.
    NotFound = 1,
    /// The partition belongs to a filesystem and cannot be acquired directly.
    ManagedByFilesystem = 2,
    /// The calling task does not have permission to access this partition.
    NotAllowed = 3,
}

/// A partition on a block device, presenting a byte-addressed subrange
/// as a full Storage interface.
#[ipc::resource(arena_size = 8, kind = 0x20, implements(storage_api::Storage))]
pub trait Partition {
    #[constructor]
    fn acquire(name: [u8; 16]) -> Result<Self, AcquireError>;

    #[message]
    fn read(&self, offset: u32, #[lease] buf: &mut [u8]) -> Result<(), StorageError>;

    #[message]
    fn write(&self, offset: u32, #[lease] buf: &[u8]) -> Result<(), StorageError>;

    #[message]
    fn erase(&self, offset: u32, len: u32) -> Result<(), StorageError>;

    #[message]
    fn program(&self, offset: u32, #[lease] buf: &[u8]) -> Result<(), StorageError>;

    #[message]
    fn geometry(&self) -> Geometry;
}

// ── FlashLayout — installed partition-table read-back ────────────────

/// Maximum number of `LayoutEntry` values returned in a single
/// `get_layout` reply. Callers paginate by incrementing `start` until
/// `start + count >= total`.
pub const MAX_ENTRIES_PER_CALL: usize = 8;

#[derive(
    Debug,
    Clone,
    Copy,
    rcard_log::Format,
    zerocopy::FromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(C, packed)]
pub struct LayoutEntry {
    pub name_hash: u32,
    pub offset: u32,
    pub size: u32,
    pub flags: u32,
}

/// A slice of the installed partition table.
///
/// `total` is the full partition count on-flash. `start` is the index of
/// the first returned entry. `count` is the number of valid entries in
/// `entries[..count]`; the rest are zeroed.
#[derive(
    Debug,
    Clone,
    Copy,
    rcard_log::Format,
    zerocopy::FromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(C, packed)]
pub struct Layout {
    pub total: u32,
    pub start: u32,
    pub count: u32,
    pub entries: [LayoutEntry; MAX_ENTRIES_PER_CALL],
}

#[derive(
    Debug,
    Clone,
    Copy,
    rcard_log::Format,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum LayoutError {
    /// The device has no valid installed firmware — either the ftab is
    /// absent/garbled or no `places.bin` footer is present at the
    /// location ftab points to. Flashing is safe from scratch.
    Unpartitioned = 0,
    /// `start` was >= the total partition count.
    OutOfRange = 1,
    /// The underlying MPI read failed.
    ReadFailure = 2,
}

/// Read-back of the partition layout installed on flash. No per-caller
/// state; all methods are static.
#[ipc::resource(arena_size = 0, kind = 0x22)]
pub trait FlashLayout {
    /// Return the installed partition table, paginated.
    ///
    /// The stub parses the ftab (at `sec_config` offset 0) to locate the
    /// on-flash `places.bin`, then reads its partition table from the
    /// footer. `start` is the index of the first partition to return.
    #[message]
    fn get_layout(start: u32) -> Result<Layout, LayoutError>;
}
