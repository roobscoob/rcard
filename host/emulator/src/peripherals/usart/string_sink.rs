use std::sync::mpsc;

use super::log::{UsartLog, UsartLogKind};
use super::UsartSink;

pub struct StringLogger {
    channel: u8,
    buf: Vec<u8>,
    tx: mpsc::Sender<UsartLog>,
}

impl StringLogger {
    pub fn new(channel: u8, tx: mpsc::Sender<UsartLog>) -> Self {
        StringLogger {
            channel,
            buf: Vec::new(),
            tx,
        }
    }
}

impl UsartSink for StringLogger {
    fn on_byte(&mut self, byte: u8) {
        if byte == b'\n' {
            let raw = std::mem::take(&mut self.buf);
            let text = String::from_utf8(raw)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
            let _ = self.tx.send(UsartLog {
                channel: self.channel,
                kind: UsartLogKind::Line(text),
            });
        } else {
            self.buf.push(byte);
        }
    }
}
