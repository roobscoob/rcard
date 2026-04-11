use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use device::adapter::{Adapter, AdapterId};
use device::device::LogSink;
use device::logs::{ControlEvent, LogEntry};
use rcard_log::LogMetadata;
use rcard_log::decoder::{Decoder, FeedResult};
use rcard_usb_proto::messages::{Awake, TunnelError};
use rcard_usb_proto::{FrameReader, ReaderError};
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

/// Read COBS-framed binary data from USART2 and push decoded events via LogSink.
async fn read_structured(mut port: tokio_serial::SerialStream, sink: LogSink) {
    let mut buf = [0u8; 1024];
    let mut cobs_buf: Vec<u8> = Vec::with_capacity(270);
    let mut streams: HashMap<u64, StreamState> = HashMap::new();
    let mut frame_reader: FrameReader<4096> = FrameReader::new();
    // Host wall-clock of the first byte of the current COBS chunk.
    // Reset on each delimiter; set when the first non-zero byte of a
    // new chunk is pushed to `cobs_buf`. Carries through to the
    // eventual structured log or control event.
    let mut chunk_start: Option<Instant> = None;

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
                    let started = chunk_start.take().unwrap_or_else(Instant::now);
                    let mut decoded = vec![0u8; cobs_buf.len()];
                    match cobs::decode(&cobs_buf, &mut decoded) {
                        Ok(len) => process_chunk(
                            &decoded[..len],
                            started,
                            &mut streams,
                            &mut frame_reader,
                            &sink,
                        ),
                        Err(_) => sink.error(crate::error::SerialError::CobsDecode),
                    }
                    cobs_buf.clear();
                }
                chunk_start = None;
            } else {
                if cobs_buf.is_empty() {
                    chunk_start = Some(Instant::now());
                }
                cobs_buf.push(byte);
                if cobs_buf.len() > 300 {
                    cobs_buf.clear();
                    chunk_start = None;
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
    /// First-byte time of the *first* chunk that contributed to this
    /// stream. Stamped onto the eventual structured log. Later chunks
    /// for the same stream do not update this — we want the earliest
    /// byte of the log's first fragment, not its last.
    start: Instant,
}

impl StreamState {
    fn new(start: Instant) -> Self {
        StreamState {
            meta_buf: [0; METADATA_SIZE],
            meta_pos: 0,
            metadata: None,
            meta_failed: false,
            decoder: Decoder::new(),
            values: Vec::new(),
            start,
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

fn process_chunk(
    chunk: &[u8],
    chunk_start: Instant,
    streams: &mut HashMap<u64, StreamState>,
    frame_reader: &mut FrameReader<4096>,
    sink: &LogSink,
) {
    if chunk.is_empty() {
        return;
    }
    match chunk[0] {
        rcard_log::wire::TYPE_LOG_FRAGMENT => {}
        rcard_log::wire::TYPE_IPC_REPLY => {
            process_ipc_reply(&chunk[1..], frame_reader, sink);
            return;
        }
        _ => return,
    }

    // Log fragment body: [stream_id: u64 LE][length: u8][data: length bytes]
    // starting at offset 1 (after the type byte).
    if chunk.len() < 1 + 9 {
        return;
    }

    let id = u64::from_le_bytes([
        chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7], chunk[8],
    ]);
    let length = chunk[9] as usize;

    if length == 0 || chunk.len() < 1 + 9 + length {
        return;
    }

    let data = &chunk[10..10 + length];
    let stream = streams
        .entry(id)
        .or_insert_with(|| StreamState::new(chunk_start));

    for &byte in data {
        if stream.feed_byte(byte) {
            if let Some(removed) = streams.remove(&id) {
                if removed.meta_failed {
                    sink.error(crate::error::SerialError::LogMetadata);
                } else if let Some(meta) = removed.metadata {
                    sink.structured_at(
                        LogEntry {
                            level: meta.level,
                            timestamp: meta.timestamp,
                            source: meta.source,
                            log_id: meta.log_id,
                            log_species: meta.log_species,
                            values: removed.values,
                        },
                        removed.start,
                    );
                }
            }
            return;
        }
    }
}

/// Feed an IPC-reply chunk's frame bytes (type byte already stripped) into
/// the per-port `FrameReader`, drain any complete frames, and dispatch
/// decoded events to the sink.
fn process_ipc_reply(frame_bytes: &[u8], reader: &mut FrameReader<4096>, sink: &LogSink) {
    // A single COBS chunk carries exactly one frame, but FrameReader
    // doesn't know that — push the bytes and drain whatever it assembles.
    reader.push(frame_bytes);

    loop {
        match reader.next_frame() {
            Ok(Some(frame)) => {
                let size = frame.header.frame_size();
                let seq = frame.header.seq;

                if let Some(simple) = frame.as_simple() {
                    // Simple frame: try to decode known opcodes, fall back
                    // to surfacing the raw opcode + payload.
                    if let Some(awake) = simple.parse::<Awake>() {
                        sink.control(ControlEvent::Awake {
                            seq,
                            uid: awake.uid,
                            firmware_id: awake.firmware_id,
                        });
                    } else if let Some(err) = simple.parse::<TunnelError>() {
                        sink.control(ControlEvent::TunnelError {
                            code: err.code,
                            seq,
                        });
                    } else {
                        sink.control(ControlEvent::UnknownSimple {
                            seq,
                            opcode: simple.opcode,
                            payload: simple.payload.to_vec(),
                        });
                    }
                } else {
                    // Non-simple frame (IPC reply body). Surface the raw
                    // payload; typed decoding of reply bodies is deferred.
                    sink.control(ControlEvent::IpcReply {
                        seq,
                        payload: frame.payload.to_vec(),
                    });
                }

                reader.consume(size);
            }
            Ok(None) => break,
            Err(ReaderError::Oversized { declared_size }) => {
                sink.control(ControlEvent::FrameError(format!(
                    "oversized frame: declared {} bytes",
                    declared_size
                )));
                reader.skip_frame(declared_size);
            }
            Err(ReaderError::Header(e)) => {
                sink.control(ControlEvent::FrameError(format!(
                    "bad frame header: {:?}",
                    e
                )));
                reader.reset();
                break;
            }
        }
    }
}
