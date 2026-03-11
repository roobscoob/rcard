use crate::formatter::Writer;

/// A writer that discards all bytes. Placeholder until real transport is wired.
pub struct NoopWriter;

impl Writer for NoopWriter {
    #[inline]
    fn write(&mut self, _bytes: &[u8]) {}
}
