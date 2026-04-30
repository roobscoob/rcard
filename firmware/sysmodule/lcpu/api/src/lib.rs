#![no_std]

#[ipc::resource(arena_size = 0, kind = 0x04)]
pub trait Time {
    #[message]
    fn get_time() -> Option<SystemDateTime>;

    #[message]
    fn set_time(dt: SystemDateTime);
}
