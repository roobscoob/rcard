use crate::messages::Message;

/// Frame magic byte.
pub const MAGIC: u8 = 0xCA;

/// Header size in bytes.
pub const HEADER_SIZE: usize = 6;

/// Wire header for every frame.
///
/// ```text
/// ┌─────────┬─────────┬──────────┬──────────┬──────────────┐
/// │ magic   │ opcode  │ seq      │ length   │ payload ...  │
/// │ 0xCA    │ u8      │ LE u16   │ LE u16   │ 0–65535      │
/// └─────────┴─────────┴──────────┴──────────┴──────────────┘
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub opcode: u8,
    pub seq: u16,
    pub length: u16,
}

/// A decoded frame: header + borrowed payload.
#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    pub header: FrameHeader,
    pub payload: &'a [u8],
}

impl<'a> Frame<'a> {
    /// Try to parse this frame as a typed message.
    ///
    /// Returns `Some(M)` if the opcode matches and the payload parses
    /// successfully, `None` otherwise.
    pub fn try_parse<M: Message>(&self) -> Option<M> {
        if self.header.opcode != M::OPCODE {
            return None;
        }
        M::from_payload(self.payload)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Buffer too short to contain a header.
    TooShort,
    /// Magic byte mismatch — framing desync.
    BadMagic,
}

impl FrameHeader {
    /// Encode this header into a 6-byte buffer.
    pub fn encode(&self, buf: &mut [u8; HEADER_SIZE]) {
        buf[0] = MAGIC;
        buf[1] = self.opcode as u8;
        buf[2..4].copy_from_slice(&self.seq.to_le_bytes());
        buf[4..6].copy_from_slice(&self.length.to_le_bytes());
    }

    /// Decode a header from a byte slice.
    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < HEADER_SIZE {
            return Err(DecodeError::TooShort);
        }
        if buf[0] != MAGIC {
            return Err(DecodeError::BadMagic);
        }
        let opcode = buf[1];
        let seq = u16::from_le_bytes([buf[2], buf[3]]);
        let length = u16::from_le_bytes([buf[4], buf[5]]);
        Ok(Self { opcode, seq, length })
    }

    /// Total frame size (header + payload).
    pub fn frame_size(&self) -> usize {
        HEADER_SIZE + self.length as usize
    }
}

/// Incrementally assemble frames from a byte stream.
///
/// Feed chunks from USB bulk reads into `push()`, then drain
/// complete frames with `next_frame()`.
pub struct FrameReader<const N: usize = 4096> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> FrameReader<N> {
    pub const fn new() -> Self {
        Self { buf: [0; N], len: 0 }
    }

    /// Append raw bytes from a USB read. Returns the number of bytes consumed.
    /// If the internal buffer is full, returns 0.
    pub fn push(&mut self, data: &[u8]) -> usize {
        let space = N - self.len;
        let n = data.len().min(space);
        self.buf[self.len..self.len + n].copy_from_slice(&data[..n]);
        self.len += n;
        n
    }

    /// Try to decode the next complete frame.
    ///
    /// Returns `Some(Frame)` if a full frame is available. The payload
    /// borrows from the internal buffer and is valid until `consume()`.
    pub fn next_frame(&self) -> Result<Option<Frame<'_>>, DecodeError> {
        if self.len < HEADER_SIZE {
            return Ok(None);
        }
        let header = FrameHeader::decode(&self.buf[..self.len])?;
        let total = header.frame_size();
        if self.len < total {
            return Ok(None);
        }
        Ok(Some(Frame {
            header,
            payload: &self.buf[HEADER_SIZE..total],
        }))
    }

    /// Remove the first frame from the buffer after processing it.
    /// Call this after `next_frame()` returns `Some`.
    pub fn consume(&mut self, frame_size: usize) {
        if frame_size >= self.len {
            self.len = 0;
        } else {
            self.buf.copy_within(frame_size.., 0);
            self.len -= frame_size;
        }
    }

    /// Discard one byte and shift — use to recover from `BadMagic`.
    pub fn skip_byte(&mut self) {
        if self.len > 0 {
            self.buf.copy_within(1.., 0);
            self.len -= 1;
        }
    }
}

/// Build frames into a buffer for USB bulk writes.
pub struct FrameWriter {
    seq: u16,
}

impl FrameWriter {
    pub const fn new() -> Self {
        Self { seq: 0 }
    }

    /// Encode a typed message into `buf`. Returns the total number of bytes
    /// written (header + payload), or `None` if the buffer is too small.
    pub fn write<M: Message>(&mut self, msg: &M, buf: &mut [u8]) -> Option<usize> {
        // Serialize payload after the header
        if buf.len() < HEADER_SIZE {
            return None;
        }
        let payload_len = msg.to_payload(&mut buf[HEADER_SIZE..])?;
        let total = HEADER_SIZE + payload_len;

        let header = FrameHeader {
            opcode: M::OPCODE,
            seq: self.seq,
            length: payload_len as u16,
        };
        self.seq = self.seq.wrapping_add(1);

        let mut hdr_buf = [0u8; HEADER_SIZE];
        header.encode(&mut hdr_buf);
        buf[..HEADER_SIZE].copy_from_slice(&hdr_buf);
        Some(total)
    }

    /// Current sequence number (for correlation).
    pub fn current_seq(&self) -> u16 {
        self.seq
    }
}
