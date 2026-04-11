#![no_std]

#[derive(
    Debug,
    Clone,
    Copy,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
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

    /// Drain buffered RX bytes into the caller's write-lease. Returns the
    /// number of bytes actually written (may be 0 if no data). Non-blocking —
    /// callers that want to wait should subscribe to the `usart_event`
    /// notification group and call `read()` from the wake handler.
    #[message]
    fn read(&self, #[lease] buf: &mut [u8]) -> u16;
}
