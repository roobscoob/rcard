use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use device::adapter::{Adapter, AdapterId};
use device::device::LogSink;
use device::logs::LogEntry;
use rcard_log::LogMetadata;
use rcard_log::decoder::{Decoder, FeedResult};
use tokio::io::AsyncReadExt;
use zerocopy::TryFromBytes;

const METADATA_SIZE: usize = core::mem::size_of::<LogMetadata>();

/// USART2 serial adapter — structured binary log stream.
///
/// Opens a serial port at 921600 baud. Reads COBS-framed binary data and
/// decodes structured log entries.
pub struct Usart2 {
    id: AdapterId,
    _reader_task: tokio::task::JoinHandle<()>,
}

impl Usart2 {
    /// Connect to a USART2 serial port.
    ///
    /// Log events are pushed into the provided `sink`.
    pub fn connect(
        port: &str,
        id: AdapterId,
        sink: LogSink,
    ) -> Result<Self, serialport::Error> {
        let stream = tokio_serial::SerialStream::open(&tokio_serial::new(port, 921_600))?;
        let task = tokio::spawn(read_structured(stream, sink));
        Ok(Usart2 {
            id,
            _reader_task: task,
        })
    }
}

impl Adapter for Usart2 {
    fn id(&self) -> AdapterId {
        self.id
    }

    fn display_name(&self) -> &str {
        "USART2"
    }

    fn capabilities(&self) -> Vec<(TypeId, Arc<dyn Any + Send + Sync>)> {
        vec![]
    }
}

impl Drop for Usart2 {
    fn drop(&mut self) {
        self._reader_task.abort();
    }
}

/// Read COBS-framed binary data from USART2 and push decoded log entries via LogSink.
async fn read_structured(mut port: tokio_serial::SerialStream, sink: LogSink) {
    let mut buf = [0u8; 1024];
    let mut cobs_buf: Vec<u8> = Vec::with_capacity(270);
    let mut streams: HashMap<u64, StreamState> = HashMap::new();

    loop {
        let n = match port.read(&mut buf).await {
            Ok(0) => {
                sink.error(crate::error::SerialError::PortClosed);
                return;
            }
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => {
                sink.error(crate::error::SerialError::Io(e));
                return;
            }
        };

        for &byte in &buf[..n] {
            if byte == 0x00 {
                if !cobs_buf.is_empty() {
                    let mut decoded = vec![0u8; cobs_buf.len()];
                    match cobs::decode(&cobs_buf, &mut decoded) {
                        Ok(len) => process_chunk(&decoded[..len], &mut streams, &sink),
                        Err(_) => sink.error(crate::error::SerialError::CobsDecode),
                    }
                    cobs_buf.clear();
                }
            } else {
                cobs_buf.push(byte);
                if cobs_buf.len() > 300 {
                    cobs_buf.clear();
                }
            }
        }
    }
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

fn process_chunk(chunk: &[u8], streams: &mut HashMap<u64, StreamState>, sink: &LogSink) {
    if chunk.len() < 9 {
        return;
    }

    let id = u64::from_le_bytes([
        chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
    ]);
    let length = chunk[8] as usize;

    if length == 0 || chunk.len() < 9 + length {
        return;
    }

    let data = &chunk[9..9 + length];
    let stream = streams.entry(id).or_insert_with(StreamState::new);

    for &byte in data {
        if stream.feed_byte(byte) {
            if let Some(removed) = streams.remove(&id) {
                if removed.meta_failed {
                    sink.error(crate::error::SerialError::LogMetadata);
                } else if let Some(meta) = removed.metadata {
                    sink.structured(LogEntry {
                        level: meta.level,
                        timestamp: meta.timestamp,
                        source: meta.source,
                        log_id: meta.log_id,
                        log_species: meta.log_species,
                        values: removed.values,
                    });
                }
            }
            return;
        }
    }
}
