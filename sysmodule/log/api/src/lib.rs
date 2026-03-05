#![no_std]

#[ipc::resource(arena_size = 0, kind = 0x03)]
pub trait Log {
    #[message]
    fn write(#[lease] data: &[u8]);
}
