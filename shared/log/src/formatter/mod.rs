pub mod builtins;
pub mod tags;
pub mod varint;

use tags::*;

pub trait Writer {
    fn write(&mut self, bytes: &[u8]);
}

impl<W: Writer> Writer for &mut W {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        (**self).write(bytes);
    }
}

pub trait Format {
    fn format<W: Writer>(&self, formatter: &mut Formatter<W>);
}

pub struct Formatter<W: Writer> {
    w: W,
}

impl<W: Writer> Formatter<W> {
    #[inline]
    pub fn new(writer: W) -> Self {
        Formatter { w: writer }
    }

    #[inline]
    pub fn into_inner(self) -> W {
        self.w
    }

    #[inline]
    pub fn write_u8(&mut self, byte: u8) {
        self.w.write(&[TAG_U8, byte]);
    }

    #[inline]
    pub fn write_i8(&mut self, byte: i8) {
        self.w.write(&[TAG_I8, byte as u8]);
    }

    #[inline]
    pub fn write_u16(&mut self, word: u16) {
        self.w.write(&[TAG_U16]);
        varint::encode_u64(&mut self.w, word as u64);
    }

    #[inline]
    pub fn write_i16(&mut self, word: i16) {
        self.w.write(&[TAG_I16]);
        varint::encode_i64(&mut self.w, word as i64);
    }

    #[inline]
    pub fn write_u32(&mut self, dword: u32) {
        self.w.write(&[TAG_U32]);
        varint::encode_u64(&mut self.w, dword as u64);
    }

    #[inline]
    pub fn write_i32(&mut self, dword: i32) {
        self.w.write(&[TAG_I32]);
        varint::encode_i64(&mut self.w, dword as i64);
    }

    #[inline]
    pub fn write_u64(&mut self, qword: u64) {
        self.w.write(&[TAG_U64]);
        varint::encode_u64(&mut self.w, qword);
    }

    #[inline]
    pub fn write_i64(&mut self, qword: i64) {
        self.w.write(&[TAG_I64]);
        varint::encode_i64(&mut self.w, qword);
    }

    #[inline]
    pub fn write_u128(&mut self, oword: u128) {
        self.w.write(&[TAG_U128]);
        varint::encode_u128(&mut self.w, oword);
    }

    #[inline]
    pub fn write_i128(&mut self, oword: i128) {
        self.w.write(&[TAG_I128]);
        varint::encode_i128(&mut self.w, oword);
    }

    #[inline]
    pub fn write_f32(&mut self, float: f32) {
        self.w.write(&[TAG_F32]);
        self.w.write(&float.to_le_bytes());
    }

    #[inline]
    pub fn write_f64(&mut self, float: f64) {
        self.w.write(&[TAG_F64]);
        self.w.write(&float.to_le_bytes());
    }

    #[inline]
    pub fn write_char(&mut self, c: char) {
        self.w.write(&[TAG_CHAR]);
        varint::encode_u64(&mut self.w, c as u64);
    }

    #[inline]
    pub fn write_bool(&mut self, boolean: bool) {
        self.w.write(&[TAG_BOOL, boolean as u8]);
    }

    #[inline]
    pub fn write_str(&mut self, s: &str) {
        self.w.write(&[TAG_STR]);
        varint::encode_u64(&mut self.w, s.len() as u64);
        self.w.write(s.as_bytes());
    }

    #[inline]
    pub fn write_unit(&mut self) {
        self.w.write(&[TAG_UNIT]);
    }

    #[inline]
    pub fn write_array<T: Format, const N: usize>(&mut self, array: &[T; N]) {
        self.w.write(&[TAG_ARRAY]);
        varint::encode_u64(&mut self.w, N as u64);
        for item in array {
            item.format(self);
        }
    }

    #[inline]
    pub fn write_slice<T: Format>(&mut self, slice: &[T]) {
        self.w.write(&[TAG_SLICE]);
        varint::encode_u64(&mut self.w, slice.len() as u64);
        for item in slice {
            item.format(self);
        }
    }

    #[inline]
    pub fn with_tuple(&mut self, type_id: u64, field_count: u64, f: impl FnOnce(&mut Self)) {
        self.w.write(&[TAG_TUPLE]);
        varint::encode_u64(&mut self.w, type_id);
        varint::encode_u64(&mut self.w, field_count);
        f(self);
    }

    #[inline]
    pub fn with_struct(&mut self, type_id: u64, field_count: u64, f: impl FnOnce(&mut Self)) {
        self.w.write(&[TAG_STRUCT]);
        varint::encode_u64(&mut self.w, type_id);
        varint::encode_u64(&mut self.w, field_count);
        f(self);
    }

    #[inline]
    pub fn write_field_id(&mut self, field_id: u64) {
        varint::encode_u64(&mut self.w, field_id);
    }

    /// Write a raw stack dump: 68-byte register header + stack bytes.
    ///
    /// Header layout (all u32 LE):
    ///   sp, stack_top, lr, pc, r0..r12, xpsr
    #[inline]
    pub fn write_stack_dump(&mut self, header: &[u8; 72], stack: &[u8]) {
        self.w.write(&[TAG_STACK_DUMP]);
        self.w.write(header);
        self.w.write(&(stack.len() as u32).to_le_bytes());
        self.w.write(stack);
    }

    #[inline]
    pub fn write_end_of_stream(&mut self) {
        self.w.write(&[TAG_END_OF_STREAM]);
    }
}

/// A writer that writes to a fixed-size byte slice, silently truncating on overflow.
pub struct SliceWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> SliceWriter<'a> {
    #[inline]
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[inline]
    pub fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn written(&self) -> &[u8] {
        &self.buf[..self.pos]
    }
}

impl Writer for SliceWriter<'_> {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let remaining = self.buf.len() - self.pos;
        let n = bytes.len().min(remaining);
        self.buf[self.pos..self.pos + n].copy_from_slice(&bytes[..n]);
        self.pos += n;
    }
}
