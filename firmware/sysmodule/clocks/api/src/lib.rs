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
pub enum Peripheral {
    Lcdc1 = 0,
    Mpi1 = 1,
    Mpi2 = 2,
    Usbc = 3,
    Trng = 4,
    I2c2 = 5,
    I2c3 = 6,
    Gptim2 = 7,
}

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
pub enum DllIndex {
    Dll2 = 1,
}

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
#[repr(C, packed)]
pub struct DllConfig {
    pub stg: u8,
    pub in_div2_en: bool,
    pub out_div2_en: bool,
}

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
pub enum ClockSource {
    Dll2 = 2,
}

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
pub enum ClocksError {
    InvalidPeripheral = 0,
    DllLockTimeout = 1,
}

#[ipc::resource(arena_size = 0, kind = 0x0A)]
pub trait Clocks {
    #[message]
    fn enable(peripheral: Peripheral) -> Result<(), ClocksError>;

    #[message]
    fn disable(peripheral: Peripheral) -> Result<(), ClocksError>;

    #[message]
    fn reset(peripheral: Peripheral) -> Result<(), ClocksError>;

    #[message]
    fn configure_dll(dll: DllIndex, config: DllConfig) -> Result<(), ClocksError>;

    #[message]
    fn set_clock_source(peripheral: Peripheral, source: ClockSource) -> Result<(), ClocksError>;

    #[message]
    fn set_divider(peripheral: Peripheral, divider: u8) -> Result<(), ClocksError>;
}
