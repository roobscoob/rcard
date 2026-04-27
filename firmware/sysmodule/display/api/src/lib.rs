#![no_std]

pub mod config;

pub use config::{DisplayConfiguration, DisplayConfigurationBuilder};

#[derive(
    Debug,
    Clone,
    Copy,
    rcard_log::Format,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum DisplayOpenError {
    AlreadyOpen = 0,
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

    #[message]
    fn set_contrast(&self, value: u8);
}
