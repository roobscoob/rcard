use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use device::adapter::{Adapter, AdapterId};
use device::device::LogSink;
use device::logs::{ControlEvent, LogEntry};
use ipc_protocol::IpcProtocol;
use rcard_log::LogMetadata;
use rcard_log::decoder::{Decoder, FeedResult};
use rcard_usb_proto::messages::{Awake, MoshiMoshi, TunnelError};
use rcard_usb_proto::{FrameReader, ReaderError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zerocopy::TryFromBytes;

const METADATA_SIZE: usize = core::mem::size_of::<LogMetadata>();

/// Maximum time a structured log stream can sit idle (no fragments arriving)
/// before the host gives up waiting for `TAG_END_OF_STREAM` and evicts it.
/// Log macros produce their fragments back-to-back; any gap this long
/// implies the producer died or bytes were lost on the wire.
const STREAM_TIMEOUT: Duration = Duration::from_secs(2);

/// How often to sweep the stream map for timeouts.
const SWEEP_INTERVAL: Duration = Duration::from_millis(500);

/// USART2 serial adapter — structured binary log stream + IPC transport.
///
/// Opens a serial port at 921600 baud. Reads COBS-framed binary data and
/// decodes structured log entries. Also exposes the unified
/// `ipc_protocol::Ipc` capability so callers can `device.get::<Ipc>()`
/// without caring about the wire underneath
/// (TYPE_IPC_REQUEST / TYPE_IPC_REPLY framing).
pub struct Usart2 {
    id: AdapterId,
    ipc: Arc<ipc_protocol::Ipc>,
    /// Same `SerialSender` the `Ipc`'s `FrameSender` uses internally —
    /// stored separately so we can expose it as its own capability for
    /// code paths that need the non-IPC-framed operations (notably a
    /// manual `send_moshi_moshi` trigger from the app UI).
    sender: Arc<SerialSender>,
    _reader_task: tokio::task::JoinHandle<()>,
}

/// Priority of the USART2 transport relative to other `ipc_protocol::Ipc`
/// providers — lower than USB so that, when both are available, USB wins.
pub const USART2_IPC_PRIORITY: u8 = 5;

/// `FrameSender` over a USART2 serial writer. COBS-wraps each frame with
/// a wire-type tag so the reader on the device side can demux it from
/// the structured log stream.
pub struct SerialSender {
    writer: tokio::sync::Mutex<tokio::io::WriteHalf<tokio_serial::SerialStream>>,
}

impl SerialSender {
    /// Common write path for any TYPE_* tagged COBS chunk. The
    /// `FrameSender` impl uses `TYPE_IPC_REQUEST`; the MoshiMoshi probe
    /// uses `TYPE_CONTROL_REQUEST`.
    async fn write_typed(&self, type_byte: u8, bytes: &[u8]) -> Result<(), String> {
        let mut raw = Vec::with_capacity(1 + bytes.len());
        raw.push(type_byte);
        raw.extend_from_slice(bytes);

        let mut encoded = vec![0u8; cobs::max_encoding_length(raw.len()) + 1];
        let enc_len = cobs::encode(&raw, &mut encoded);
        encoded[enc_len] = 0x00;

        let mut writer = self.writer.lock().await;
        writer
            .write_all(&encoded[..enc_len + 1])
            .await
            .map_err(|e| e.to_string())
    }

    /// Send a `MoshiMoshi` simple frame on the control-request channel.
    /// `sysmodule_log` responds with an `Awake` simple frame carrying the
    /// chip UID + firmware build id — the same payload it sends once at
    /// boot — so the host can identify devices that were already running
    /// when this serial port opened.
    pub async fn send_moshi_moshi(&self) -> Result<(), String> {
        // header(5) + opcode(1) — `MoshiMoshi` has no payload.
        let mut buf = [0u8; 6];
        let n = rcard_usb_proto::simple::encode_simple(&MoshiMoshi, &mut buf, 0)
            .ok_or_else(|| "encode_simple(MoshiMoshi) failed".to_string())?;
        self.write_typed(rcard_log::wire::TYPE_CONTROL_REQUEST, &buf[..n])
            .await
    }
}

impl ipc_protocol::FrameSender for SerialSender {
    fn send_frame<'a>(
        &'a self,
        bytes: Vec<u8>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>
    {
        Box::pin(async move {
            self.write_typed(rcard_log::wire::TYPE_IPC_REQUEST, &bytes)
                .await
        })
    }
}

impl Usart2 {
    /// Connect to a USART2 serial port.
    ///
    /// Log events are pushed into the provided `sink`. The returned adapter
    /// exposes the unified `ipc_protocol::Ipc` capability for making IPC
    /// calls over USART2.
    pub fn connect(
        port: &str,
        id: AdapterId,
        sink: LogSink,
    ) -> Result<Self, serialport::Error> {
        let stream = tokio_serial::SerialStream::open(&tokio_serial::new(port, 921_600))?;
        let (reader, writer) = tokio::io::split(stream);

        let protocol = Arc::new(IpcProtocol::new());
        let raw_sender = Arc::new(SerialSender {
            writer: tokio::sync::Mutex::new(writer),
        });
        let sender: Arc<dyn ipc_protocol::FrameSender> = raw_sender.clone();
        let ipc = Arc::new(ipc_protocol::Ipc::new(
            "usart2",
            USART2_IPC_PRIORITY,
            protocol.clone(),
            sender,
        ));

        let task = tokio::spawn(read_structured(reader, sink, protocol));

        // Probe device identity now that the wire is open. The `Awake`
        // response lands through the read path and surfaces as a
        // `ControlEvent::Awake`, same as the boot-time sentinel — so a
        // device that was already running before this port opened still
        // gets identified. Spawned because `connect` is sync; the probe
        // is fire-and-forget (failure just delays detection until either
        // a real `Awake` shows up or the user reconnects).
        eprintln!("[usart2:{port}] discovery: sending MoshiMoshi");
        let probe_sender = raw_sender.clone();
        let probe_port = port.to_string();
        tokio::spawn(async move {
            if let Err(e) = probe_sender.send_moshi_moshi().await {
                eprintln!(
                    "[usart2:{probe_port}] discovery: MoshiMoshi send failed: {e}"
                );
            }
        });

        Ok(Usart2 {
            id,
            ipc,
            sender: raw_sender,
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
        vec![
            (TypeId::of::<ipc_protocol::Ipc>(), self.ipc.clone()),
            (TypeId::of::<SerialSender>(), self.sender.clone()),
        ]
    }
}

impl Drop for Usart2 {
    fn drop(&mut self) {
        self._reader_task.abort();
    }
}

/// Read COBS-framed binary data from USART2 and push decoded events via LogSink.
async fn read_structured(
    mut port: tokio::io::ReadHalf<tokio_serial::SerialStream>,
    sink: LogSink,
    protocol: Arc<IpcProtocol>,
) {
    let mut buf = [0u8; 1024];
    let mut cobs_buf: Vec<u8> = Vec::with_capacity(270);
    let mut streams: HashMap<u64, StreamState> = HashMap::new();
    let mut frame_reader: FrameReader<4096> = FrameReader::new();
    // Host wall-clock of the first byte of the current COBS chunk.
    // Reset on each delimiter; set when the first non-zero byte of a
    // new chunk is pushed to `cobs_buf`. Carries through to the
    // eventual structured log or control event.
    let mut chunk_start: Option<Instant> = None;

    let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let n = tokio::select! {
            read_result = port.read(&mut buf) => match read_result {
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
            },
            _ = sweep.tick() => {
                sweep_stale_streams(&mut streams, &sink);
                continue;
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
                            &protocol,
                        ).await,
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
    /// Updated every time a fragment is fed into this stream. The
    /// sweep task evicts streams whose `last_activity` is older than
    /// `STREAM_TIMEOUT`.
    last_activity: Instant,
    /// Running count of bytes fed into this stream (excluding the
    /// terminator). Reported in `SerialError::StreamTimeout` to help
    /// diagnose where the producer got stuck.
    bytes_seen: usize,
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
            last_activity: start,
            bytes_seen: 0,
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

async fn process_chunk(
    chunk: &[u8],
    chunk_start: Instant,
    streams: &mut HashMap<u64, StreamState>,
    frame_reader: &mut FrameReader<4096>,
    sink: &LogSink,
    protocol: &Arc<IpcProtocol>,
) {
    if chunk.is_empty() {
        return;
    }
    match chunk[0] {
        rcard_log::wire::TYPE_LOG_FRAGMENT => {}
        rcard_log::wire::TYPE_IPC_REPLY => {
            process_ipc_reply(&chunk[1..], frame_reader, sink, protocol).await;
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
    stream.last_activity = chunk_start;
    stream.bytes_seen += data.len();

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
                            truncated: false,
                        },
                        removed.start,
                    );
                }
            }
            return;
        }
    }
}

/// Evict any streams whose `last_activity` is older than `STREAM_TIMEOUT`.
///
/// Each evicted stream is reported via `SerialError::StreamTimeout`. If the
/// metadata header was fully received before the stall, the partially decoded
/// values are also emitted as a `LogEntry` with `truncated: true` so the UI
/// can surface them with a visual indicator instead of dropping them on the
/// floor.
fn sweep_stale_streams(
    streams: &mut HashMap<u64, StreamState>,
    sink: &LogSink,
) {
    let now = Instant::now();
    let stale: Vec<u64> = streams
        .iter()
        .filter(|(_, s)| now.duration_since(s.last_activity) > STREAM_TIMEOUT)
        .map(|(id, _)| *id)
        .collect();

    for id in stale {
        let removed = match streams.remove(&id) {
            Some(s) => s,
            None => continue,
        };
        let log_id = removed.metadata.as_ref().map(|m| m.log_id).unwrap_or(0);
        let age_ms = now.duration_since(removed.start).as_millis() as u64;
        sink.error(crate::error::SerialError::StreamTimeout {
            stream_id: id,
            log_id,
            bytes_decoded: removed.bytes_seen,
            age_ms,
        });
        if let Some(meta) = removed.metadata {
            sink.structured_at(
                LogEntry {
                    level: meta.level,
                    timestamp: meta.timestamp,
                    source: meta.source,
                    log_id: meta.log_id,
                    log_species: meta.log_species,
                    values: removed.values,
                    truncated: true,
                },
                removed.start,
            );
        }
    }
}

/// Feed an IPC-reply chunk's frame bytes (type byte already stripped) into
/// the per-port `FrameReader`, drain any complete frames, and dispatch
/// decoded events.
///
/// If the reply matches a pending IPC call (by sequence number), it's
/// resolved via the protocol. Otherwise it's dispatched as a ControlEvent
/// to the serial panel (tunnel errors, awake, unsolicited frames).
async fn process_ipc_reply(
    frame_bytes: &[u8],
    reader: &mut FrameReader<4096>,
    sink: &LogSink,
    protocol: &Arc<IpcProtocol>,
) {
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
                        // Tunnel errors can be responses to pending IPC
                        // calls (e.g. TaskDead) or unsolicited (e.g. from
                        // a peer). Try protocol resolution first.
                        let resolved = ipc_protocol::ResolvedResponse::TunnelError(err.code);
                        let matched = protocol.resolve(seq, resolved).await;
                        if !matched {
                            sink.control(ControlEvent::TunnelError {
                                code: err.code,
                                seq,
                            });
                        }
                    } else {
                        sink.control(ControlEvent::UnknownSimple {
                            seq,
                            opcode: simple.opcode,
                            payload: simple.payload.to_vec(),
                        });
                    }
                } else if let Some(response) = frame.as_ipc_response() {
                    // IPC reply or tunnel error for a pending call.
                    // Try to resolve against the protocol first. If it
                    // matches a pending request, the oneshot is fulfilled
                    // and we're done. Otherwise fall through to the
                    // ControlEvent path for the serial panel.
                    let resolved = if let Some(reply) = response.as_reply() {
                        ipc_protocol::ResolvedResponse::Reply(ipc_protocol::IpcCallResult {
                            rc: reply.rc,
                            return_value: reply.return_value.to_vec(),
                            lease_writeback: reply.lease_writeback.to_vec(),
                        })
                    } else if let Some(err) = response.parse_simple::<TunnelError>() {
                        ipc_protocol::ResolvedResponse::TunnelError(err.code)
                    } else {
                        ipc_protocol::ResolvedResponse::UnexpectedFrame
                    };

                    let matched = protocol.resolve(seq, resolved).await;
                    if !matched {
                        // Unsolicited IPC reply — surface to the panel.
                        sink.control(ControlEvent::IpcReply {
                            seq,
                            payload: frame.payload.to_vec(),
                        });
                    }
                } else {
                    // Unknown frame type — surface to the panel.
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
