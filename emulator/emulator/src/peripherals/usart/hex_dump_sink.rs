use super::UsartSink;

pub struct HexDumpSink {
    channel: u8,
    buf: Vec<u8>,
}

impl HexDumpSink {
    pub fn new(channel: u8) -> Self {
        HexDumpSink {
            channel,
            buf: Vec::new(),
        }
    }
}

impl UsartSink for HexDumpSink {
    fn on_byte(&mut self, byte: u8) {
        self.buf.push(byte);
        if self.buf.len() >= 32 {
            let hex: Vec<String> = self.buf.iter().map(|b| format!("{b:02x}")).collect();
            eprintln!("[USART{}] {}", self.channel, hex.join(" "));
            self.buf.clear();
        }
    }
}

impl Drop for HexDumpSink {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            let hex: Vec<String> = self.buf.iter().map(|b| format!("{b:02x}")).collect();
            eprintln!("[USART{}] {}", self.channel, hex.join(" "));
        }
    }
}
