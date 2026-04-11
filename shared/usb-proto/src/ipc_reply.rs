use crate::frame::FrameType;
use crate::header::{FrameHeader, HEADER_SIZE};

// IpcReply fixed fields: rc(4) + reply_len(1)
const FIXED_FIELDS_SIZE: usize = 5;

/// Zero-copy parsed view of an IPC reply payload.
#[derive(Clone, Debug)]
pub struct IpcReplyView<'a> {
    /// Kernel response code.
    pub rc: u32,
    /// The serialized return value.
    pub return_value: &'a [u8],
    /// Concatenated writeback data for Write and ReadWrite leases.
    pub lease_writeback: &'a [u8],
}

impl<'a> IpcReplyView<'a> {
    /// Parse an IPC reply from a payload byte slice.
    pub fn from_bytes(payload: &'a [u8]) -> Option<Self> {
        if payload.len() < FIXED_FIELDS_SIZE {
            return None;
        }

        let rc = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        let reply_len_wire = payload[4];
        let reply_len = reply_len_wire as usize + 1; // wire = actual - 1

        let return_start = FIXED_FIELDS_SIZE;
        if payload.len() < return_start + reply_len {
            return None;
        }

        let return_value = &payload[return_start..return_start + reply_len];
        let lease_writeback = &payload[return_start + reply_len..];

        Some(Self {
            rc,
            return_value,
            lease_writeback,
        })
    }

    /// Try to parse the return value as a zerocopy type.
    pub fn parse<T: zerocopy::TryFromBytes + zerocopy::KnownLayout + zerocopy::Immutable>(
        &self,
    ) -> Option<T> {
        zerocopy::TryFromBytes::try_read_from_prefix(self.return_value)
            .ok()
            .map(|(v, _)| v)
    }
}

/// Builder for encoding an IPC reply frame.
pub struct IpcReply<'a> {
    pub rc: u32,
    pub return_value: &'a [u8],
    pub lease_writeback: &'a [u8],
}

impl IpcReply<'_> {
    /// Encode this reply into `buf` with the given sequence number.
    ///
    /// Empty return values are allowed — a single padding byte is written
    /// on the wire so the `reply_len - 1` encoding stays representable.
    ///
    /// Returns the total number of bytes written, or `None` if the buffer
    /// is too small or the return value exceeds 256 bytes.
    pub fn encode_into(&self, buf: &mut [u8], seq: u16) -> Option<usize> {
        if self.return_value.len() > 256 {
            return None;
        }

        // On the wire, return value is always at least 1 byte.
        let wire_return_len = self.return_value.len().max(1);

        let payload_len = FIXED_FIELDS_SIZE + wire_return_len + self.lease_writeback.len();

        if payload_len > u16::MAX as usize {
            return None;
        }

        let total = HEADER_SIZE + payload_len;
        if buf.len() < total {
            return None;
        }

        // Header
        let header = FrameHeader {
            frame_type: FrameType::IpcReply,
            seq,
            length: payload_len as u16,
        };
        let mut hdr = [0u8; HEADER_SIZE];
        header.encode(&mut hdr);
        buf[..HEADER_SIZE].copy_from_slice(&hdr);

        // Fixed fields
        let p = &mut buf[HEADER_SIZE..];
        p[0..4].copy_from_slice(&self.rc.to_le_bytes());
        p[4] = (wire_return_len - 1) as u8;

        // Return value (pad with a zero byte if empty)
        let mut offset = FIXED_FIELDS_SIZE;
        if self.return_value.is_empty() {
            p[offset] = 0;
        } else {
            p[offset..offset + self.return_value.len()].copy_from_slice(self.return_value);
        }
        offset += wire_return_len;

        // Lease writeback
        p[offset..offset + self.lease_writeback.len()].copy_from_slice(self.lease_writeback);

        Some(total)
    }
}
