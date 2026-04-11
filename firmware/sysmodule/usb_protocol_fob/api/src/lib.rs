#![no_std]

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
)]
#[repr(u8)]
pub enum FobSendError {
    /// USB is not connected or not configured.
    Disconnected = 0,
    /// The encode buffer is too small for this message.
    BufferFull = 1,
}

#[ipc::resource(arena_size = 0, kind = 0x33)]
pub trait UsbProtocolFob {
    /// Send a SimpleFrame to the host on the fob-driven channel.
    #[message]
    fn send(opcode: u8, #[lease] payload: &[u8]) -> Result<(), FobSendError>;
}
