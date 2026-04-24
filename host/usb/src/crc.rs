//! Frame-level CRC16 wrap/unwrap — mirrors the firmware-side logic in
//! `firmware/sysmodule/usb_protocol_host/src/main.rs` and
//! `firmware/sysmodule/usb_protocol_fob/src/main.rs`. The wire format
//! for each USB bulk transfer is:
//!
//! ```text
//! [IPC frame bytes (header + payload)][CRC16 (2 bytes, big-endian)][optional 1-byte pad]
//! ```
//!
//! The CRC is over the frame bytes only (not the pad). The pad is
//! appended iff `(frame.len() + 2) % 64 == 0`, so the bulk transfer
//! always ends with a short packet (< 64 bytes) — that short packet is
//! our frame boundary. On CRC mismatch the receiver discards the whole
//! transfer and NACKs via `TunnelErrorCode::RequestCorrupted`; the next
//! bulk transfer is, by USB definition, a fresh frame, so resync is
//! structural rather than heuristic.

const MAX_PACKET_SIZE: usize = 64;

/// CRC-16/CCITT-FALSE: poly=0x1021, init=0xFFFF, no reflection, xorout=0.
pub(crate) fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Wrap an IPC frame for transmission: append big-endian CRC16, plus a
/// 1-byte pad when the wire would otherwise be a multiple of 64.
pub(crate) fn wrap_frame(frame: &[u8]) -> Vec<u8> {
    let crc = crc16(frame);
    let needs_pad = (frame.len() + 2) % MAX_PACKET_SIZE == 0;
    let total = frame.len() + 2 + if needs_pad { 1 } else { 0 };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(frame);
    out.extend_from_slice(&crc.to_be_bytes());
    if needs_pad {
        out.push(0);
    }
    out
}

/// Unwrap a completed bulk IN transfer buffer into the IPC frame bytes.
/// Locates the CRC at `[5 + header.length, 5 + header.length + 2)` (the
/// header's `length` field tells us exactly where the frame ends — any
/// corruption in the length field fails the CRC check and the whole
/// buffer is rejected, so trusting the length here is safe).
pub(crate) fn unwrap_frame(buf: &[u8]) -> Result<&[u8], CrcError> {
    const HEADER_SIZE: usize = 5;
    if buf.len() < HEADER_SIZE + 2 {
        return Err(CrcError::TooShort);
    }
    // Header layout mirrors `rcard_usb_proto::FrameHeader`:
    //   [frame_type: u8][seq: u16 LE][length: u16 LE]
    let length = u16::from_le_bytes([buf[3], buf[4]]) as usize;
    let frame_size = HEADER_SIZE + length;
    if frame_size + 2 > buf.len() {
        return Err(CrcError::TooShort);
    }
    let expected = u16::from_be_bytes([buf[frame_size], buf[frame_size + 1]]);
    let actual = crc16(&buf[..frame_size]);
    if expected != actual {
        return Err(CrcError::Mismatch);
    }
    Ok(&buf[..frame_size])
}

#[derive(Debug)]
pub(crate) enum CrcError {
    TooShort,
    Mismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid frame: `[frame_type=0x01 (IpcRequest), seq=0, length=N][payload N bytes]`.
    fn synth_frame(payload: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        f.push(0x01);
        f.extend_from_slice(&0u16.to_le_bytes()); // seq
        f.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn roundtrip_empty_payload() {
        let frame = synth_frame(&[]);
        let wrapped = wrap_frame(&frame);
        assert_eq!(unwrap_frame(&wrapped).unwrap(), &frame[..]);
    }

    #[test]
    fn roundtrip_small() {
        let frame = synth_frame(b"hello");
        let wrapped = wrap_frame(&frame);
        assert_eq!(unwrap_frame(&wrapped).unwrap(), &frame[..]);
    }

    #[test]
    fn pad_appended_when_wire_is_mps_multiple() {
        // Frame size chosen so frame.len() + 2 == 64: frame = 62 bytes,
        // which needs payload = 57.
        let frame = synth_frame(&[0xAB; 57]);
        assert_eq!(frame.len(), 62);
        let wrapped = wrap_frame(&frame);
        assert_eq!(wrapped.len(), 65); // 62 + 2 CRC + 1 pad
        assert_eq!(unwrap_frame(&wrapped).unwrap(), &frame[..]);
    }

    #[test]
    fn no_pad_when_wire_is_not_mps_multiple() {
        let frame = synth_frame(&[0xCD; 10]);
        let wrapped = wrap_frame(&frame);
        assert_eq!(wrapped.len(), frame.len() + 2); // no pad
        assert_eq!(unwrap_frame(&wrapped).unwrap(), &frame[..]);
    }

    #[test]
    fn unwrap_ignores_trailing_pad() {
        // Even if the buffer has extra bytes past the CRC, unwrap should
        // only check bytes [0 .. frame_size + 2).
        let frame = synth_frame(b"data");
        let mut wrapped = wrap_frame(&frame);
        wrapped.push(0xFF);
        wrapped.push(0xFF);
        assert_eq!(unwrap_frame(&wrapped).unwrap(), &frame[..]);
    }

    #[test]
    fn detects_bitflip_in_frame() {
        let frame = synth_frame(b"payload");
        let mut wrapped = wrap_frame(&frame);
        wrapped[6] ^= 0x01;
        assert!(matches!(unwrap_frame(&wrapped), Err(CrcError::Mismatch)));
    }

    #[test]
    fn detects_bitflip_in_length() {
        // A flipped length bit is rejected — either as `Mismatch` (if the
        // corrupted length still leaves enough buffer for a CRC check) or
        // as `TooShort` (if the corrupted length now exceeds the buffer).
        // Both outcomes are caught by the receiver as "discard and NACK"
        // so we accept either here.
        let frame = synth_frame(b"xxxxxx");
        let mut wrapped = wrap_frame(&frame);
        wrapped[3] ^= 0x01;
        assert!(unwrap_frame(&wrapped).is_err());
    }
}
