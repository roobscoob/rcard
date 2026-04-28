use super::{Formatter, Writer};

impl<W: Writer> Formatter<W> {
    #[inline]
    pub(super) fn encode_u128(&mut self, mut value: u128) {
        loop {
            let byte = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                self.w.write(&[byte]);
                return;
            }
            self.w.write(&[byte | 0x80]);
        }
    }

    #[inline]
    pub(super) fn encode_u64(&mut self, mut value: u64) {
        loop {
            let byte = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                self.w.write(&[byte]);
                return;
            }
            self.w.write(&[byte | 0x80]);
        }
    }

    #[inline]
    pub(super) fn encode_i64(&mut self, value: i64) {
        let zigzag = ((value << 1) ^ (value >> 63)) as u64;
        self.encode_u64(zigzag);
    }

    #[inline]
    pub(super) fn encode_i128(&mut self, value: i128) {
        let zigzag = ((value << 1) ^ (value >> 127)) as u128;
        self.encode_u128(zigzag);
    }
}
