#![no_std]

extern crate self as rcard_log;

use core::sync::atomic::AtomicBool;

/// Set to `true` by `rcard_log::panic!` before calling `core::panic!`.
/// The panic handler checks this: if set, the message was already logged
/// through the structured pipeline and no further logging is needed.
pub static PANIC_LOGGED: AtomicBool = AtomicBool::new(false);

pub mod formatter;
mod log_level;
mod metadata;
pub mod wire;

pub use log_level::LogLevel;
pub use metadata::LogMetadata;
pub use rcard_log_macros::Format;
#[doc(hidden)]
pub use rcard_log_macros::__species;

#[cfg(feature = "writer")]
mod log_writer;
#[cfg(feature = "writer")]
mod macros;
#[cfg(feature = "writer")]
mod noop_writer;

#[cfg(feature = "writer")]
mod unwrap;

#[cfg(feature = "writer")]
pub use log_writer::LogWriter;
#[cfg(feature = "writer")]
pub use noop_writer::NoopWriter;
#[cfg(feature = "writer")]
pub use unwrap::{OptionExt, ResultExt};

#[cfg(feature = "writer")]
pub mod stack_dump;

#[cfg(feature = "alloc")]
pub mod decoder;
#[cfg(feature = "alloc")]
mod owned;
#[cfg(feature = "alloc")]
pub use owned::OwnedValue;
