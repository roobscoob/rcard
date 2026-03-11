use super::UsartSink;

pub struct NullSink;

impl UsartSink for NullSink {
    fn on_byte(&mut self, _byte: u8) {}
}
