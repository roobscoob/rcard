#![no_std]

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
pub enum UsartOpenError {
    ReservedUsart = 0,
    InvalidIndex = 1,
    AlreadyOpen = 2,
}

#[ipc::resource(arena_size = 3, kind = 0x01)]
pub trait Usart {
    #[constructor]
    fn open(index: u8) -> Result<Self, UsartOpenError>;

    #[message]
    fn write(&self, #[lease] data: &[u8]);
}
