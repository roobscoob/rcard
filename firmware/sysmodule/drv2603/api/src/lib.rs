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
pub enum Drv2603Error {
    InvalidStrength = 0,
}

#[ipc::resource(arena_size = 0, kind = 0x0D)]
pub trait Drv2603 {
    /// Drive the haptic motor at the given strength (0.0 = off, 1.0 = max).
    /// Clamped to `0.0..=1.0`; maps to 50%–95.5% PWM duty on GPTIM2 CH1.
    #[message]
    fn drive(strength: f32) -> Result<(), Drv2603Error>;

    #[message]
    fn stop() -> Result<(), Drv2603Error>;
}
