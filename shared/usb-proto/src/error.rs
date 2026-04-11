/// Error decoding a frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderError {
    /// Not enough bytes to decode the header.
    TooShort,
    /// The `frame_type` byte is not a recognized value.
    BadFrameType(u8),
}

#[cfg(feature = "std")]
impl core::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooShort => f.write_str("not enough bytes for frame header"),
            Self::BadFrameType(t) => write!(f, "unrecognized frame type: {:#04x}", t),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for HeaderError {}
