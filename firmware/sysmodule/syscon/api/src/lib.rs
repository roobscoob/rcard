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
pub enum Ldo {
    Vdd33Ldo3 = 0,
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
pub enum SysconError {
    InvalidLdo = 0,
}

#[ipc::resource(arena_size = 0, kind = 0x0B)]
pub trait Syscon {
    #[message]
    fn enable_ldo(ldo: Ldo) -> Result<(), SysconError>;

    #[message]
    fn disable_ldo(ldo: Ldo) -> Result<(), SysconError>;
}
