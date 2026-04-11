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
}

impl fmt::Display for SerialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "serial I/O error: {e}"),
            Self::PortClosed => write!(f, "serial port closed"),
            Self::CobsDecode => write!(f, "COBS frame decode failed"),
            Self::LogMetadata => write!(f, "log metadata deserialization failed"),
        }
    }
}

impl std::error::Error for SerialError {}
