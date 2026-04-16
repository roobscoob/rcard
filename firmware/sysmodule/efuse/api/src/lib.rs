#![no_std]

use rcard_log::Format;

#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum EfuseError {
    /// `bank_id` was not in the range 0..=3.
    InvalidBank = 0,
}

#[ipc::resource(arena_size = 0, kind = 0x06)]
pub trait Efuse {
    /// Read one full 256-bit eFuse bank (banks 0..=3) as a 32-byte array.
    ///
    /// The controller's read sequence is run synchronously and the eight
    /// 32-bit bank words are serialized little-endian into the return
    /// buffer. Readback masking (bank0 fuse bits 244..=253) is applied
    /// by the hardware — a masked bank reads as all zeros.
    #[message]
    fn read(bank_id: u8) -> Result<[u8; 32], EfuseError>;
}
