mod hex_dump_sink;
pub mod log;
mod null_sink;
mod string_sink;
mod structured_sink;

pub use hex_dump_sink::HexDumpSink;
pub use null_sink::NullSink;
pub use string_sink::StringLogger;
pub use structured_sink::StructuredSink;

pub trait UsartSink: Send + 'static {
    fn on_byte(&mut self, byte: u8);
}
