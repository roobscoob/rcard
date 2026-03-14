#![no_std]

#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(u8)]
pub enum BlockErrorKind {
    OutOfRange = 0,
    Device = 1,
}

#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(C, packed)]
pub struct BlockError {
    pub kind: BlockErrorKind,
    pub device_code: u16,
}

impl BlockError {
    pub fn out_of_range() -> Self {
        Self {
            kind: BlockErrorKind::OutOfRange,
            device_code: 0,
        }
    }
    pub fn device(code: u16) -> Self {
        Self {
            kind: BlockErrorKind::Device,
            device_code: code,
        }
    }
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
