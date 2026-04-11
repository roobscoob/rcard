use crate::error::HeaderError;
use crate::frame::FrameType;

/// Size of the common frame header in bytes.
pub const HEADER_SIZE: usize = 5;

/// Common header for all frame types.
///
/// ```text
/// [frame_type: u8][seq: u16 LE][length: u16 LE]
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub frame_type: FrameType,
    pub seq: u16,
    /// Number of payload bytes following this header.
    pub length: u16,
}

impl FrameHeader {
    /// Total frame size (header + payload).
    pub fn frame_size(&self) -> usize {
        HEADER_SIZE + self.length as usize
    }

    /// Encode this header into a 5-byte buffer.
    pub fn encode(&self, buf: &mut [u8; HEADER_SIZE]) {
        buf[0] = self.frame_type as u8;
        buf[1..3].copy_from_slice(&self.seq.to_le_bytes());
        buf[3..5].copy_from_slice(&self.length.to_le_bytes());
    }

    /// Decode a header from a byte slice.
    pub fn decode(buf: &[u8]) -> Result<Self, HeaderError> {
        if buf.len() < HEADER_SIZE {
            return Err(HeaderError::TooShort);
        }
        let frame_type = FrameType::from_u8(buf[0])?;
        let seq = u16::from_le_bytes([buf[1], buf[2]]);
        let length = u16::from_le_bytes([buf[3], buf[4]]);
        Ok(Self {
            frame_type,
            seq,
            length,
        })
    }
}
