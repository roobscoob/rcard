use crate::frame::FrameType;
use crate::header::{FrameHeader, HEADER_SIZE};
use crate::messages::Message;

/// Zero-copy parsed view of a simple frame payload.
#[derive(Clone, Copy, Debug)]
pub struct SimpleFrameView<'a> {
    pub opcode: u8,
    pub payload: &'a [u8],
}

impl<'a> SimpleFrameView<'a> {
    /// Parse a simple frame from a payload byte slice.
    pub fn from_bytes(payload: &'a [u8]) -> Option<Self> {
        if payload.is_empty() {
            return None;
        }
        Some(Self {
            opcode: payload[0],
            payload: &payload[1..],
        })
    }

    /// Try to decode this frame as a typed message.
    ///
    /// Returns `None` if the opcode doesn't match `M::OPCODE` or the
    /// payload doesn't parse.
    pub fn parse<M: Message>(&self) -> Option<M> {
        if self.opcode != M::OPCODE {
            return None;
        }
        M::from_payload(self.payload)
    }
}

/// Encode a simple frame carrying a typed [`Message`] into `buf`.
///
/// Returns the total number of bytes written, or `None` if the buffer
/// is too small or the message payload doesn't fit.
pub fn encode_simple<M: Message>(msg: &M, buf: &mut [u8], seq: u16) -> Option<usize> {
    // Opcode byte + message payload
    if buf.len() < HEADER_SIZE + 1 {
        return None;
    }
    let payload_start = HEADER_SIZE + 1; // skip header + opcode
    let msg_len = msg.to_payload(&mut buf[payload_start..])?;
    let payload_len = 1 + msg_len; // opcode + message bytes

    if payload_len > u16::MAX as usize {
        return None;
    }

    // Write header
    let header = FrameHeader {
        frame_type: FrameType::Simple,
        seq,
        length: payload_len as u16,
    };
    let mut hdr = [0u8; HEADER_SIZE];
    header.encode(&mut hdr);
    buf[..HEADER_SIZE].copy_from_slice(&hdr);
    buf[HEADER_SIZE] = M::OPCODE;

    Some(HEADER_SIZE + payload_len)
}

/// Encode a simple frame from a raw opcode and payload.
///
/// Returns the total number of bytes written, or `None` if the buffer
/// is too small.
pub fn encode_simple_raw(opcode: u8, payload: &[u8], buf: &mut [u8], seq: u16) -> Option<usize> {
    let payload_len = 1 + payload.len();

    if payload_len > u16::MAX as usize {
        return None;
    }

    let total = HEADER_SIZE + payload_len;
    if buf.len() < total {
        return None;
    }

    let header = FrameHeader {
        frame_type: FrameType::Simple,
        seq,
        length: payload_len as u16,
    };
    let mut hdr = [0u8; HEADER_SIZE];
    header.encode(&mut hdr);
    buf[..HEADER_SIZE].copy_from_slice(&hdr);
    buf[HEADER_SIZE] = opcode;
    buf[HEADER_SIZE + 1..total].copy_from_slice(payload);

    Some(total)
}
