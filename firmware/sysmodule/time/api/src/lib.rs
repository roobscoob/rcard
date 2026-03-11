#![no_std]

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, hubpack::SerializedSize,
)]
pub struct SystemDateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

#[ipc::resource(arena_size = 0, kind = 0x04)]
pub trait Time {
    #[message]
    fn get_time() -> Option<SystemDateTime>;

    #[message]
    fn set_time(dt: SystemDateTime);
}
