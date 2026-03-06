#![no_std]

pub use storage_api::Storage;

#[derive(serde::Serialize, serde::Deserialize, hubpack::SerializedSize, Debug)]
pub enum SdmmcOpenError {
    ReservedSlot,
    InitFailed,
}

/// Concrete SDMMC resource. arena_size = 1 because there's one SD card slot.
#[ipc::resource(arena_size = 1, kind = 0x11, implements(storage_api::Storage))]
pub trait Sdmmc {
    #[constructor]
    fn open() -> Result<Self, SdmmcOpenError>;

    #[message]
    fn read_block(&self, block: u32, #[lease] buf: &mut [u8]);

    #[message]
    fn write_block(&self, block: u32, #[lease] buf: &[u8]);

    #[message]
    fn block_count(&self) -> u32;
}
