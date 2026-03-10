#![no_std]

#[ipc::resource(arena_size = 0, kind = 0x06)]
pub trait Compositor {
    #[message]
    fn ping() -> u32;
}
