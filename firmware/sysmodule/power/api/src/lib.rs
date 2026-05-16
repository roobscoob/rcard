#![no_std]

use rcard_log::Format;

// ── LDO types ───────────────────────────────────────────────────────────────

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
    Vdd33Ldo2 = 0,
    Vdd33Ldo3 = 1,
}

// ── Charger types ───────────────────────────────────────────────────────────

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
    Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum ChargerState {
    Off = 0,
    PowerUp = 1,
    Idle = 2,
    PreConstantCurrent = 3,
    ConstantCurrent = 4,
    ConstantVoltage = 5,
    EndOfCharge = 6,
    Unknown = 7,
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
pub struct ChargerStatus {
    pub state: ChargerState,
    pub vbus_present: bool,
}

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
    Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum ChargerEvent {
    VbusConnected = 0,
    VbusDisconnected = 1,
    ChargingStarted = 2,
    ChargingComplete = 3,
    BatteryHigh = 4,
    StateChanged = 5,
}

// ── Errors ──────────────────────────────────────────────────────────────────

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
pub enum PowerError {
    InvalidLdo = 0,
    ChargerNotCalibrated = 1,
    InvalidParameter = 2,
    EfuseReadFailed = 3,
}

// ── IPC trait ───────────────────────────────────────────────────────────────

#[ipc::resource(arena_size = 0, kind = 0x0B)]
pub trait Power {
    #[message]
    fn enable_ldo(ldo: Ldo) -> Result<(), PowerError>;

    #[message]
    fn disable_ldo(ldo: Ldo) -> Result<(), PowerError>;

    #[message]
    fn charger_status() -> ChargerStatus;

    #[message]
    fn charger_force_start() -> Result<(), PowerError>;

    #[message]
    fn charger_force_stop() -> Result<(), PowerError>;
}
