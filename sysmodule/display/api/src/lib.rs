#![no_std]

pub mod config;

pub use config::{DisplayConfiguration, DisplayConfigurationBuilder};

#[derive(serde::Serialize, serde::Deserialize, hubpack::SerializedSize, Debug)]
pub enum DisplayOpenError {
    AlreadyOpen,
}

#[ipc::resource(arena_size = 1, kind = 0x02)]
pub trait Display {
    #[constructor]
    fn open(config: DisplayConfiguration) -> Result<Self, DisplayOpenError>;

    /// Write a full framebuffer to the display. The buffer is in SSD1312
    /// GDDRAM format: `height/8` pages of `width` bytes each, where each byte
    /// encodes 8 vertical pixels with the LSB as the topmost pixel.
    /// For a 128x64 display this is 1024 bytes.
    #[message]
    fn draw(&self, #[lease] framebuffer: &[u8]);
}
