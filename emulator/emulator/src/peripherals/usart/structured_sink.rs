use std::collections::HashMap;
use std::sync::mpsc;

use hubpack::SerializedSize;
use rcard_log::decoder::{Decoder, FeedResult};
use rcard_log::{LogMetadata, OwnedValue};

use super::log::{LogStream, UsartLog, UsartLogKind};
use super::UsartSink;

const METADATA_SIZE: usize = LogMetadata::MAX_SIZE;

pub struct StructuredSink {
    channel: u8,
    frame: FrameState,
    streams: HashMap<u64, StreamState>,
    tx: mpsc::Sender<UsartLog>,
}

enum FrameState {
    ReadingId { buf: [u8; 8], pos: u8 },
    ReadingLength { id: u64 },
    ReadingData { id: u64, remaining: u8 },
}

struct StreamState {
    meta_buf: [u8; METADATA_SIZE],
    meta_pos: usize,
    decoder: Decoder,
    tx: Option<mpsc::Sender<OwnedValue>>,
    channel: u8,
    entry_tx: mpsc::Sender<UsartLog>,
}

impl StreamState {
    fn new(channel: u8, entry_tx: mpsc::Sender<UsartLog>) -> Self {
        StreamState {
            meta_buf: [0; METADATA_SIZE],
            meta_pos: 0,
            decoder: Decoder::new(),
            tx: None,
            channel,
            entry_tx,
        }
    }

    /// Feed a byte into this stream. Returns `true` if end-of-stream was reached.
    fn feed_byte(&mut self, byte: u8) -> bool {
        if self.meta_pos < METADATA_SIZE {
            self.meta_buf[self.meta_pos] = byte;
            self.meta_pos += 1;

            if self.meta_pos == METADATA_SIZE {
                let (metadata, _) = hubpack::deserialize::<LogMetadata>(&self.meta_buf)
                    .expect("failed to deserialize LogMetadata");
                let (tx, rx) = mpsc::channel();
                self.tx = Some(tx);
                let _ = self.entry_tx.send(UsartLog {
                    channel: self.channel,
                    kind: UsartLogKind::Stream(LogStream {
                        metadata,
                        values: rx,
                    }),
                });
            }
            return false;
        }

        let tx = match &self.tx {
            Some(tx) => tx,
            None => return false,
        };

        let (_, result) = self.decoder.feed(&[byte]);
        match result {
            FeedResult::Done(value) => {
                let _ = tx.send(value);
                false
            }
            FeedResult::EndOfStream => true,
            _ => false,
        }
    }
}

impl StructuredSink {
    pub fn new(channel: u8, tx: mpsc::Sender<UsartLog>) -> Self {
        StructuredSink {
            channel,
            frame: FrameState::ReadingId {
                buf: [0; 8],
                pos: 0,
            },
            streams: HashMap::new(),
            tx,
        }
    }
}

impl UsartSink for StructuredSink {
    fn on_byte(&mut self, byte: u8) {
        let frame = std::mem::replace(
            &mut self.frame,
            FrameState::ReadingId {
                buf: [0; 8],
                pos: 0,
            },
        );

        self.frame = match frame {
            FrameState::ReadingId { mut buf, pos } => {
                buf[pos as usize] = byte;
                let pos = pos + 1;
                if pos == 8 {
                    FrameState::ReadingLength {
                        id: u64::from_le_bytes(buf),
                    }
                } else {
                    FrameState::ReadingId { buf, pos }
                }
            }
            FrameState::ReadingLength { id } => {
                if byte == 0 {
                    FrameState::ReadingId {
                        buf: [0; 8],
                        pos: 0,
                    }
                } else {
                    FrameState::ReadingData {
                        id,
                        remaining: byte,
                    }
                }
            }
            FrameState::ReadingData { id, remaining } => {
                let stream = self
                    .streams
                    .entry(id)
                    .or_insert_with(|| StreamState::new(self.channel, self.tx.clone()));

                if stream.feed_byte(byte) {
                    self.streams.remove(&id);
                }

                let remaining = remaining - 1;
                if remaining == 0 {
                    FrameState::ReadingId {
                        buf: [0; 8],
                        pos: 0,
                    }
                } else {
                    FrameState::ReadingData { id, remaining }
                }
            }
        };
    }
}
