use std::fmt;

/// Errors from the serial adapter's reader tasks.
#[derive(Debug)]
pub enum SerialError {
    /// Serial port I/O error (fatal — reader exits).
    Io(std::io::Error),
    /// Port closed (read returned 0 bytes, fatal — reader exits).
    PortClosed,
    /// COBS frame decode failed — frame skipped.
    CobsDecode,
    /// Log metadata deserialization failed — entry skipped.
    LogMetadata,
    /// A structured log stream went idle without ever receiving a
    /// `TAG_END_OF_STREAM` terminator — the host evicted it to prevent
    /// the per-stream state from leaking. Any bytes that were decoded
    /// before eviction are emitted as a `LogEntry` with `truncated: true`.
    StreamTimeout {
        stream_id: u64,
        log_id: u64,
        bytes_decoded: usize,
        age_ms: u64,
    },
}

impl fmt::Display for SerialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "serial I/O error: {e}"),
            Self::PortClosed => write!(f, "serial port closed"),
            Self::CobsDecode => write!(f, "COBS frame decode failed"),
            Self::LogMetadata => write!(f, "log metadata deserialization failed"),
            Self::StreamTimeout { stream_id, log_id, bytes_decoded, age_ms } => write!(
                f,
                "structured log stream {stream_id} (log_id={log_id}) timed out after \
                 {age_ms}ms with {bytes_decoded} bytes decoded — no EoS received"
            ),
        }
    }
}

impl std::error::Error for SerialError {}
