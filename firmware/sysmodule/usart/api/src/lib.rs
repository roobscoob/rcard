#![no_std]

#[derive(serde::Serialize, serde::Deserialize, hubpack::SerializedSize, Debug)]
pub enum UsartOpenError {
    ReservedUsart,
    InvalidIndex,
    AlreadyOpen,
}

#[ipc::resource(arena_size = 3, kind = 0x01)]
pub trait Usart {
    #[constructor]
    fn open(index: u8) -> Result<Self, UsartOpenError>;

    #[message]
    fn write(&self, #[lease] data: &[u8]);
}
