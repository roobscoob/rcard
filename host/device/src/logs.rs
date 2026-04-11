use rcard_log::{LogLevel, OwnedValue};

use crate::adapter::AdapterId;

/// A log event with its source adapter.
#[derive(Clone, Debug)]
pub struct Log {
    pub adapter: AdapterId,
    pub contents: LogContents,
}

/// The content of a log event.
#[derive(Clone, Debug)]
pub enum LogContents {
    /// Decoded structured log entry (from a binary stream like USART2).
    Structured(LogEntry),
    /// Plain text line (from a text stream like USART1).
    Text(String),
    /// Named auxiliary text stream (e.g. "renode").
    Auxiliary { name: String, text: String },
    /// Renode emulator log with a parsed level and message.
    Renode { level: LogLevel, message: String },
}

/// A decoded structured log entry from a binary stream.
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub level: LogLevel,
    pub timestamp: u64,
    /// Task index on the device.
    pub source: u16,
    /// Unique monotonic ID for this log entry.
    pub log_id: u64,
    /// Species hash — key into the tfw metadata for format string + source location.
    pub log_species: u64,
    /// Decoded argument values from the binary payload.
    pub values: Vec<OwnedValue>,
}
