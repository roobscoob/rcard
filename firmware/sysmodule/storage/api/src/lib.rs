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
