#![no_std]

pub use storage_api::{BlockError, Storage};

#[derive(
    Debug,
    Clone,
    Copy,
    rcard_log::Format,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(u8)]
pub enum SdmmcOpenError {
    ReservedSlot = 0,
    InitFailed = 1,
}

/// Concrete SDMMC resource. arena_size = 1 because there's one SD card slot.
#[ipc::resource(arena_size = 1, kind = 0x11, implements(storage_api::Storage))]
pub trait Sdmmc {
    #[constructor]
    fn open() -> Result<Self, SdmmcOpenError>;

    #[message]
    fn read_block(&self, block: u32, #[lease] buf: &mut [u8]) -> Result<(), BlockError>;

    #[message]
    fn write_block(&self, block: u32, #[lease] buf: &[u8]) -> Result<(), BlockError>;

    #[message]
    fn block_count(&self) -> u32;
}
