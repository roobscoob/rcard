use std::sync::mpsc;

use super::log::{UsartLog, UsartLogKind};
use super::UsartSink;

pub struct StringLogger {
    channel: u8,
    buf: String,
    tx: mpsc::Sender<UsartLog>,
}

impl StringLogger {
    pub fn new(channel: u8, tx: mpsc::Sender<UsartLog>) -> Self {
        StringLogger {
            channel,
            buf: String::new(),
            tx,
        }
    }
}

impl UsartSink for StringLogger {
    fn on_byte(&mut self, byte: u8) {
        if byte == b'\n' {
            let _ = self.tx.send(UsartLog {
                channel: self.channel,
                kind: UsartLogKind::Line(std::mem::take(&mut self.buf)),
            });
        } else {
            self.buf.push(byte as char);
        }
    }
}
