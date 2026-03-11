#![no_std]

pub mod ring;
pub use storage_api::{BlockError, Storage};

include!(concat!(env!("OUT_DIR"), "/partition_names.rs"));

#[derive(serde::Serialize, serde::Deserialize, hubpack::SerializedSize, Debug)]
pub enum AcquireError {
    /// The partition is already acquired by another task.
    InUse,
    /// No partition with this name exists.
    NotFound,
    /// The partition belongs to a filesystem and cannot be acquired directly.
    ManagedByFilesystem,
    /// The calling task does not have permission to access this partition.
    NotAllowed,
}

/// A partition on a block device, presenting a subrange of blocks
/// as a full Storage interface.
#[ipc::resource(arena_size = 8, kind = 0x20, implements(storage_api::Storage))]
pub trait Partition {
    #[constructor]
    fn acquire(name: [u8; 16]) -> Result<Self, AcquireError>;

    #[message]
    fn read_block(&self, block: u32, #[lease] buf: &mut [u8]) -> Result<(), BlockError>;

    #[message]
    fn write_block(&self, block: u32, #[lease] buf: &[u8]) -> Result<(), BlockError>;

    #[message]
    fn block_count(&self) -> u32;
}
