pub mod sifli_debug;

use std::collections::HashMap;

use engine::Backend;
use engine::logs::{LogEntry, Logs, Usart1Line};
use rcard_log::LogMetadata;
use rcard_log::decoder::{Decoder, FeedResult};
use sifli_debug::{DebugHandle, TapReader};
use tokio::io::AsyncReadExt;
use tokio::sync::broadcast;
use zerocopy::TryFromBytes;

const METADATA_SIZE: usize = core::mem::size_of::<LogMetadata>();

/// Unmanaged debug connection over two serial ports (USART1 + USART2).
pub struct Serial {
    logs: SerialLogs,
}

struct SerialLogs {
    structured_tx: broadcast::Sender<LogEntry>,
    hypervisor_tx: broadcast::Sender<Usart1Line>,
    debug_handle: Option<DebugHandle>,
}

impl Serial {
    /// Connect to a device over two serial ports.
    ///
    /// - `usart1`: hypervisor/supervisor text stream (1M baud)
    /// - `usart2`: structured binary log stream (115200 baud)
    ///
    /// Either or both may be `None` to skip that stream.
    pub fn connect(usart1: Option<&str>, usart2: Option<&str>) -> Result<Self, serialport::Error> {
        let (structured_tx, _) = broadcast::channel(256);
        let (hypervisor_tx, _) = broadcast::channel(256);

        let debug_handle = if let Some(port) = usart1 {
            let stream = tokio_serial::SerialStream::open(&tokio_serial::new(port, 1_000_000))?;
            let (reader, writer) = tokio::io::split(stream);
            let (handle, tap_reader, _tap_writer) = sifli_debug::tap(reader, writer);
            let tx = hypervisor_tx.clone();
            tokio::spawn(read_hypervisor(tap_reader, tx));
            Some(handle)
        } else {
            None
        };

        if let Some(port) = usart2 {
            let stream = tokio_serial::SerialStream::open(&tokio_serial::new(port, 115_200))?;
            let tx = structured_tx.clone();
            tokio::spawn(read_structured(stream, tx));
        }

        Ok(Self {
            logs: SerialLogs {
                structured_tx,
                hypervisor_tx,
                debug_handle,
            },
        })
    }

    /// Returns the SifliDebug handle, if USART1 is connected.
    pub fn debug_handle(&self) -> Option<&DebugHandle> {
        self.logs.debug_handle.as_ref()
    }
}

/// Read UTF-8 lines from USART1 and broadcast them as hypervisor lines.
async fn read_hypervisor(mut reader: TapReader, tx: broadcast::Sender<Usart1Line>) {
    let mut buf = [0u8; 1024];
    let mut line = String::new();

    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) => return,
            Ok(n) => n,
            Err(_) => return,
        };

        for &byte in &buf[..n] {
            if byte == b'\n' {
                let text = std::mem::take(&mut line);
                let _ = tx.send(Usart1Line { text });
            } else {
                line.push(byte as char);
            }
        }
    }
}

/// Read framed binary data from USART2 and broadcast decoded log entries.
async fn read_structured(mut port: tokio_serial::SerialStream, tx: broadcast::Sender<LogEntry>) {
    let mut buf = [0u8; 1024];
    let mut frame = FrameState::ReadingId {
        buf: [0; 8],
        pos: 0,
    };
    let mut streams: HashMap<u64, StreamState> = HashMap::new();

    loop {
        let n = match port.read(&mut buf).await {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(_) => return,
        };

        for &byte in &buf[..n] {
            process_byte(byte, &mut frame, &mut streams, &tx);
        }
    }
}

enum FrameState {
    ReadingId { buf: [u8; 8], pos: u8 },
    ReadingLength { id: u64 },
    ReadingData { id: u64, remaining: u8 },
}

struct StreamState {
    meta_buf: [u8; METADATA_SIZE],
    meta_pos: usize,
    metadata: Option<LogMetadata>,
    decoder: Decoder,
    values: Vec<rcard_log::OwnedValue>,
}

impl StreamState {
    fn new() -> Self {
        StreamState {
            meta_buf: [0; METADATA_SIZE],
            meta_pos: 0,
            metadata: None,
            decoder: Decoder::new(),
            values: Vec::new(),
        }
    }

    /// Feed a byte. Returns `true` if end-of-stream was reached.
    fn feed_byte(&mut self, byte: u8) -> bool {
        if self.meta_pos < METADATA_SIZE {
            self.meta_buf[self.meta_pos] = byte;
            self.meta_pos += 1;

            if self.meta_pos == METADATA_SIZE {
                let metadata = LogMetadata::try_read_from_bytes(&self.meta_buf)
                    .expect("failed to deserialize LogMetadata");
                self.metadata = Some(metadata);
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

fn process_byte(
    byte: u8,
    frame: &mut FrameState,
    streams: &mut HashMap<u64, StreamState>,
    tx: &broadcast::Sender<LogEntry>,
) {
    let prev = std::mem::replace(
        frame,
        FrameState::ReadingId {
            buf: [0; 8],
            pos: 0,
        },
    );

    *frame = match prev {
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
            let stream = streams.entry(id).or_insert_with(StreamState::new);

            if stream.feed_byte(byte) {
                // End of stream — emit the log entry.
                if let Some(removed) = streams.remove(&id) {
                    if let Some(meta) = removed.metadata {
                        let _ = tx.send(LogEntry {
                            level: meta.level,
                            timestamp: meta.timestamp,
                            source: meta.source,
                            log_id: meta.log_id,
                            log_species: meta.log_species,
                            values: removed.values,
                        });
                    }
                }
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

impl Backend for Serial {
    fn logs(&self) -> &dyn Logs {
        &self.logs
    }
}

impl Logs for SerialLogs {
    fn subscribe_structured(&self) -> broadcast::Receiver<LogEntry> {
        self.structured_tx.subscribe()
    }

    fn subscribe_usart1(&self) -> broadcast::Receiver<Usart1Line> {
        self.hypervisor_tx.subscribe()
    }
}
