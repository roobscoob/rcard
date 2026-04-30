#![no_std]

#[ipc::resource(arena_size = 1, kind = 0x17)]
pub trait Lcpu {
    #[constructor]
    fn init() -> Result<Self, ()>;

    #[message]
    fn send_data(#[lease] data: &[u8]);
}
