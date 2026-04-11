use crate::frame::FrameType;
use crate::header::{FrameHeader, HEADER_SIZE};

/// Maximum IPC argument size in bytes.
pub const MAX_ARGS: usize = 256;

/// Maximum lease data pool size in bytes (shared across all leases).
pub const LEASE_POOL_SIZE: usize = 8192;

/// Size of a packed lease descriptor on the wire.
pub const LEASE_DESCRIPTOR_SIZE: usize = 2;

/// Maximum value representable in the 14-bit length field.
pub const MAX_LEASE_LENGTH: usize = (1 << 14) - 1;

// IpcRequest fixed fields: task_id(2) + resource_kind(1) + method(1) + lease_count(1) + args_len(1)
const FIXED_FIELDS_SIZE: usize = 6;

/// Lease direction — determines what data flows in the request vs. reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LeaseKind {
    /// `&[u8]` — data sent in request, nothing returned.
    Read = 0,
    /// Return-only `&mut [u8]` — no data in request, data in reply.
    Write = 1,
    /// `&mut [u8]` with initial contents — data in request AND reply.
    ReadWrite = 2,
}

impl LeaseKind {
    pub fn from_bits(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Read),
            1 => Some(Self::Write),
            2 => Some(Self::ReadWrite),
            _ => None,
        }
    }

    /// Whether this lease carries data in the request.
    pub fn has_request_data(&self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    /// Whether this lease carries data in the reply.
    pub fn has_reply_data(&self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

/// Packed lease descriptor: `[kind:2 | length:14]` as `u16 LE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeaseDescriptor {
    pub kind: LeaseKind,
    pub length: u16,
}

impl LeaseDescriptor {
    /// Decode from a 2-byte wire representation.
    pub fn from_wire(wire: u16) -> Option<Self> {
        let kind_bits = (wire >> 14) as u8;
        let length = wire & 0x3FFF;
        Some(Self {
            kind: LeaseKind::from_bits(kind_bits)?,
            length,
        })
    }

    /// Encode to the 2-byte wire representation.
    ///
    /// Returns `None` if `length` exceeds the 14-bit maximum (16383).
    pub fn to_wire(&self) -> Option<u16> {
        if self.length > MAX_LEASE_LENGTH as u16 {
            return None;
        }
        Some(((self.kind as u16) << 14) | self.length)
    }
}

/// Zero-copy parsed view of an IPC request payload.
///
/// Lease descriptors are parsed lazily from the borrowed payload on each
/// access — no static array, no cap on lease count.
#[derive(Clone, Copy, Debug)]
pub struct IpcRequestView<'a> {
    pub task_id: u16,
    pub resource_kind: u8,
    pub method: u8,
    lease_count: u8,
    /// Raw descriptor bytes: `lease_count * 2` bytes.
    descriptors: &'a [u8],
    args: &'a [u8],
    lease_data: &'a [u8],
}

impl<'a> IpcRequestView<'a> {
    /// Parse an IPC request from a payload byte slice.
    pub fn from_bytes(payload: &'a [u8]) -> Option<Self> {
        if payload.len() < FIXED_FIELDS_SIZE {
            return None;
        }

        let task_id = u16::from_le_bytes([payload[0], payload[1]]);
        let resource_kind = payload[2];
        let method = payload[3];
        let lease_count = payload[4];
        // Wire encodes actual_len - 1, so 0..=255 maps to 1..=256.
        let args_len = payload[5] as usize + 1;

        let descriptors_start = FIXED_FIELDS_SIZE;
        let descriptors_size = lease_count as usize * LEASE_DESCRIPTOR_SIZE;
        let args_start = descriptors_start + descriptors_size;

        if payload.len() < args_start + args_len {
            return None;
        }

        let descriptors = &payload[descriptors_start..descriptors_start + descriptors_size];
        let args = &payload[args_start..args_start + args_len];
        let lease_data = &payload[args_start + args_len..];

        // Validate that lease data is large enough for all Read/ReadWrite
        // descriptors. We walk the raw descriptor bytes without storing
        // them — the actual parsing happens lazily on access.
        let mut expected_lease_data = 0usize;
        for i in 0..lease_count as usize {
            let off = i * LEASE_DESCRIPTOR_SIZE;
            let wire = u16::from_le_bytes([descriptors[off], descriptors[off + 1]]);
            let desc = LeaseDescriptor::from_wire(wire)?;
            if desc.kind.has_request_data() {
                expected_lease_data += desc.length as usize;
            }
        }
        if lease_data.len() < expected_lease_data {
            return None;
        }

        Some(Self {
            task_id,
            resource_kind,
            method,
            lease_count,
            descriptors,
            args,
            lease_data,
        })
    }

    pub fn lease_count(&self) -> usize {
        self.lease_count as usize
    }

    pub fn args(&self) -> &'a [u8] {
        self.args
    }

    /// Parse a lease descriptor by index from the raw wire bytes.
    pub fn lease(&self, index: usize) -> Option<LeaseDescriptor> {
        if index >= self.lease_count as usize {
            return None;
        }
        let offset = index * LEASE_DESCRIPTOR_SIZE;
        let wire = u16::from_le_bytes([
            self.descriptors[offset],
            self.descriptors[offset + 1],
        ]);
        LeaseDescriptor::from_wire(wire)
    }

    /// Get the request-side data for a Read or ReadWrite lease.
    ///
    /// Returns `None` for Write leases or out-of-bounds indices.
    pub fn lease_data(&self, index: usize) -> Option<&'a [u8]> {
        let desc = self.lease(index)?;
        if !desc.kind.has_request_data() {
            return None;
        }

        // Sum data lengths of preceding Read/ReadWrite leases.
        let mut data_offset = 0usize;
        for i in 0..index {
            if let Some(prev) = self.lease(i) {
                if prev.kind.has_request_data() {
                    data_offset += prev.length as usize;
                }
            }
        }

        let end = data_offset + desc.length as usize;
        if end > self.lease_data.len() {
            return None;
        }
        Some(&self.lease_data[data_offset..end])
    }
}

/// Builder for encoding an IPC request frame.
pub struct IpcRequest<'a> {
    pub task_id: u16,
    pub resource_kind: u8,
    pub method: u8,
    pub args: &'a [u8],
    pub leases: &'a [LeaseDescriptor],
    /// One slice per Read/ReadWrite lease, in order.
    pub lease_data: &'a [&'a [u8]],
}

impl IpcRequest<'_> {
    /// Encode this request into `buf` with the given sequence number.
    ///
    /// Empty args are allowed — a single padding byte is written on the
    /// wire so the `args_len - 1` encoding stays representable as u8.
    ///
    /// Returns the total number of bytes written (header + payload),
    /// or `None` if the buffer is too small or args exceed 256 bytes.
    pub fn encode_into(&self, buf: &mut [u8], seq: u16) -> Option<usize> {
        if self.args.len() > MAX_ARGS {
            return None;
        }
        if self.leases.len() > u8::MAX as usize {
            return None;
        }

        // lease_data must have exactly one slice per Read/ReadWrite lease,
        // and each slice's length must match its descriptor's declared length.
        let mut data_idx = 0;
        for desc in self.leases {
            desc.to_wire()?; // validate length fits in 14 bits
            if desc.kind.has_request_data() {
                let slice = self.lease_data.get(data_idx)?;
                if slice.len() != desc.length as usize {
                    return None;
                }
                data_idx += 1;
            }
        }
        if data_idx != self.lease_data.len() {
            return None;
        }

        // On the wire, args is always at least 1 byte.
        let wire_args_len = self.args.len().max(1);

        let lease_count = self.leases.len();
        let descriptors_size = lease_count * LEASE_DESCRIPTOR_SIZE;
        let lease_data_size: usize = self.lease_data.iter().map(|s| s.len()).sum();

        let payload_len =
            FIXED_FIELDS_SIZE + descriptors_size + wire_args_len + lease_data_size;

        if payload_len > u16::MAX as usize {
            return None;
        }

        let total = HEADER_SIZE + payload_len;
        if buf.len() < total {
            return None;
        }

        // Header
        let header = FrameHeader {
            frame_type: FrameType::IpcRequest,
            seq,
            length: payload_len as u16,
        };
        let mut hdr = [0u8; HEADER_SIZE];
        header.encode(&mut hdr);
        buf[..HEADER_SIZE].copy_from_slice(&hdr);

        // Fixed fields
        let p = &mut buf[HEADER_SIZE..];
        p[0..2].copy_from_slice(&self.task_id.to_le_bytes());
        p[2] = self.resource_kind;
        p[3] = self.method;
        p[4] = lease_count as u8;
        p[5] = (wire_args_len - 1) as u8;

        // Lease descriptors (already validated by the check above)
        let mut offset = FIXED_FIELDS_SIZE;
        for desc in self.leases {
            let wire = desc.to_wire().unwrap_or(0);
            p[offset..offset + 2].copy_from_slice(&wire.to_le_bytes());
            offset += LEASE_DESCRIPTOR_SIZE;
        }

        // Args (pad with a zero byte if empty)
        if self.args.is_empty() {
            p[offset] = 0;
        } else {
            p[offset..offset + self.args.len()].copy_from_slice(self.args);
        }
        offset += wire_args_len;

        // Lease data
        for data in self.lease_data {
            p[offset..offset + data.len()].copy_from_slice(data);
            offset += data.len();
        }

        Some(total)
    }
}
