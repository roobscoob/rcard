#![no_std]

use crate::channel::ImageFormat;

pub mod channel;

#[derive(
    Debug,
    Clone,
    Copy,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(u8)]
pub enum FrameBufferError {
    WrongSeedLength = 0,
    OutOfVram = 1,
    NotOwner = 2,
}

#[ipc::resource(arena_size = 32, kind = 0x06)]
pub trait FrameBuffer {
    #[constructor]
    fn new(format: ImageFormat, #[lease] seed_data: &[u8]) -> Result<Self, FrameBufferError>;

    #[message]
    fn write(&mut self, #[lease] seed_data: &[u8]) -> Result<(), FrameBufferError>;
}
