use std::collections::HashMap;
use std::sync::mpsc;

use device::logs::LogEntry;
use rcard_log::LogMetadata;
use rcard_log::decoder::{Decoder, FeedResult};
use zerocopy::TryFromBytes;

use super::log::{UsartLog, UsartLogKind};
use super::UsartSink;

const METADATA_SIZE: usize = core::mem::size_of::<LogMetadata>();

pub struct StructuredSink {
    channel: u8,
    cobs_buf: Vec<u8>,
    streams: HashMap<u64, StreamState>,
    tx: mpsc::Sender<UsartLog>,
}

struct StreamState {
    meta_buf: [u8; METADATA_SIZE],
    meta_pos: usize,
    metadata: Option<LogMetadata>,
    meta_failed: bool,
    decoder: Decoder,
    values: Vec<rcard_log::OwnedValue>,
}

impl StreamState {
    fn new() -> Self {
        StreamState {
            meta_buf: [0; METADATA_SIZE],
            meta_pos: 0,
            metadata: None,
            meta_failed: false,
            decoder: Decoder::new(),
            values: Vec::new(),
        }
    }

    fn feed_byte(&mut self, byte: u8) -> bool {
        if self.meta_pos < METADATA_SIZE {
            self.meta_buf[self.meta_pos] = byte;
            self.meta_pos += 1;

            if self.meta_pos == METADATA_SIZE {
                match LogMetadata::try_read_from_bytes(&self.meta_buf) {
                    Ok(metadata) => self.metadata = Some(metadata),
                    Err(_) => self.meta_failed = true,
                }
            }
            return false;
        }

        let (_, result) = self.decoder.feed(&[byte]);
        match result {
            FeedResult::Done(value) => {
                self.values.push(value);
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
            cobs_buf: Vec::with_capacity(270),
            streams: HashMap::new(),
            tx,
        }
    }

    fn process_chunk(&mut self, chunk: &[u8]) {
        // First byte is the type discriminator (see rcard_log::wire).
        // Non-log types (IPC replies etc.) share this wire but aren't
        // handled by the structured-log sink — skip them silently.
        if chunk.is_empty() {
            return;
        }
        match chunk[0] {
            rcard_log::wire::TYPE_LOG_FRAGMENT => {}
            _ => return,
        }

        if chunk.len() < 1 + 9 {
            return;
        }

        let id = u64::from_le_bytes([
            chunk[1], chunk[2], chunk[3], chunk[4],
            chunk[5], chunk[6], chunk[7], chunk[8],
        ]);
        let length = chunk[9] as usize;

        if length == 0 || chunk.len() < 1 + 9 + length {
            return;
        }

        let data = &chunk[10..10 + length];
        let stream = self.streams.entry(id).or_insert_with(StreamState::new);

        for &byte in data {
            if stream.feed_byte(byte) {
                if let Some(removed) = self.streams.remove(&id) {
                    if !removed.meta_failed {
                        if let Some(meta) = removed.metadata {
                            let _ = self.tx.send(UsartLog {
                                channel: self.channel,
                                kind: UsartLogKind::Stream(super::log::LogStream {
                                    metadata: meta,
                                    values: removed.values,
                                }),
                            });
                        }
                    }
                }
                return;
            }
        }
    }
}

impl UsartSink for StructuredSink {
    fn on_byte(&mut self, byte: u8) {
        if byte == 0x00 {
            if !self.cobs_buf.is_empty() {
                let mut decoded = vec![0u8; self.cobs_buf.len()];
                if let Ok(len) = cobs::decode(&self.cobs_buf, &mut decoded) {
                    self.process_chunk(&decoded[..len]);
                }
                self.cobs_buf.clear();
            }
        } else {
            self.cobs_buf.push(byte);
            if self.cobs_buf.len() > 300 {
                self.cobs_buf.clear();
            }
        }
    }
}
