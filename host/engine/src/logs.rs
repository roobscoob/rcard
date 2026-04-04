use rcard_log::{LogLevel, OwnedValue};
use tokio::sync::broadcast;

/// A decoded structured log entry from the primary stream (USART2).
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

/// A line from the hypervisor stream (USART1). Plain UTF-8 text.
#[derive(Clone, Debug)]
pub struct Usart1Line {
    pub text: String,
}

/// Access to the device's log streams.
pub trait Logs: Send + Sync {
    /// Subscribe to decoded structured log entries.
    fn subscribe_structured(&self) -> broadcast::Receiver<LogEntry>;

    /// Subscribe to hypervisor (plain text) lines.
    fn subscribe_usart1(&self) -> broadcast::Receiver<Usart1Line>;

    /// Backend-specific named text streams (e.g. "renode" for emulator output).
    /// Returns the names of available auxiliary streams.
    fn auxiliary_streams(&self) -> &[&str] { &[] }

    /// Subscribe to an auxiliary text stream by name.
    fn subscribe_auxiliary(&self, _name: &str) -> Option<broadcast::Receiver<String>> { None }
}
