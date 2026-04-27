use std::sync::mpsc;

use super::usart::log::{UsartLog, UsartLogKind};
use super::usart::UsartSink;

const GDDRAM_SIZE: usize = 1024;

pub struct DisplaySink {
    channel: u8,
    buf: Vec<u8>,
    tx: mpsc::Sender<UsartLog>,
}

impl DisplaySink {
    pub fn new(channel: u8, tx: mpsc::Sender<UsartLog>) -> Self {
        DisplaySink {
            channel,
            buf: Vec::with_capacity(GDDRAM_SIZE),
            tx,
        }
    }
}

impl UsartSink for DisplaySink {
    fn on_byte(&mut self, byte: u8) {
        self.buf.push(byte);
        if self.buf.len() == GDDRAM_SIZE {
            let frame = std::mem::replace(&mut self.buf, Vec::with_capacity(GDDRAM_SIZE));
            let _ = self.tx.send(UsartLog {
                channel: self.channel,
                kind: UsartLogKind::Display(frame),
            });
        }
    }
}
