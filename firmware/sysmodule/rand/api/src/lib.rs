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
pub enum RandError {
    /// The TRNG seed generation did not complete in time.
    SeedTimeout = 0,
    /// The TRNG random number generation did not complete in time.
    GenerateTimeout = 1,
}

#[ipc::resource(arena_size = 0, kind = 0x09)]
pub trait Rand {
    /// Generate 32 bytes (256 bits) of random data from the hardware TRNG.
    ///
    /// Seeds the TRNG engine, waits for seed generation, then triggers
    /// random number generation and returns the full 256-bit output.
    #[message]
    fn generate() -> Result<[u8; 32], RandError>;
}
