#![no_std]

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, hubpack::SerializedSize)]
pub enum BlockError {
    OutOfRange,
    Device(u16),
}

/// Interface-only trait: any block storage device.
/// No arena_size = interface, generates StorageDyn client wrapper.
#[ipc::interface(kind = 0x10)]
pub trait Storage {
    #[message]
    fn read_block(&self, block: u32, #[lease] buf: &mut [u8]) -> Result<(), BlockError>;

    #[message]
    fn write_block(&self, block: u32, #[lease] buf: &[u8]) -> Result<(), BlockError>;

    #[message]
    fn block_count(&self) -> u32;
}
