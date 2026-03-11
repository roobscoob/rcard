use super::Writer;

/// Encode an unsigned 128-bit integer as LEB128 into a writer.
#[inline]
pub fn encode_u128(w: &mut impl Writer, mut value: u128) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            w.write(&[byte]);
            return;
        }
        w.write(&[byte | 0x80]);
    }
}

/// Encode an unsigned 64-bit integer as LEB128 into a writer.
#[inline]
pub fn encode_u64(w: &mut impl Writer, mut value: u64) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            w.write(&[byte]);
            return;
        }
        w.write(&[byte | 0x80]);
    }
}

/// Zigzag-encode a signed 64-bit integer, then LEB128.
#[inline]
pub fn encode_i64(w: &mut impl Writer, value: i64) {
    let zigzag = ((value << 1) ^ (value >> 63)) as u64;
    encode_u64(w, zigzag);
}

/// Zigzag-encode a signed 128-bit integer, then LEB128.
#[inline]
pub fn encode_i128(w: &mut impl Writer, value: i128) {
    let zigzag = ((value << 1) ^ (value >> 127)) as u128;
    encode_u128(w, zigzag);
}
