use std::fmt;

/// Errors from the USB adapter's reader tasks.
#[derive(Debug)]
pub enum UsbError {
    /// USB bulk transfer failed (fatal — reader exits).
    Transfer(nusb::transfer::TransferError),
    /// Frame header was malformed.
    BadFrameHeader(rcard_usb_proto::ReaderError),
    /// Frame's declared size exceeds the buffer — skipped.
    FrameOversize { declared_size: usize },
    /// Log metadata deserialization failed — entry skipped.
    LogMetadata,
    /// Value decoder error in log payload — entry skipped.
    LogDecode,
}

impl fmt::Display for UsbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transfer(e) => write!(f, "USB transfer failed: {e}"),
            Self::BadFrameHeader(e) => write!(f, "malformed frame header: {e}"),
            Self::FrameOversize { declared_size } => {
                write!(f, "oversized frame ({declared_size} bytes) skipped")
            }
            Self::LogMetadata => write!(f, "log metadata deserialization failed"),
            Self::LogDecode => write!(f, "log value decode error"),
        }
    }
}

impl std::error::Error for UsbError {}
