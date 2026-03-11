#![no_std]

pub mod formatter;
mod log_level;
mod metadata;

pub use log_level::LogLevel;
pub use metadata::LogMetadata;
pub use rcard_log_macros::Format;

#[cfg(feature = "writer")]
mod log_writer;
#[cfg(feature = "writer")]
mod macros;
#[cfg(feature = "writer")]
mod noop_writer;

#[cfg(feature = "writer")]
pub use log_writer::LogWriter;
#[cfg(feature = "writer")]
pub use noop_writer::NoopWriter;

#[cfg(feature = "alloc")]
pub mod decoder;
#[cfg(feature = "alloc")]
mod owned;
#[cfg(feature = "alloc")]
pub use owned::OwnedValue;
