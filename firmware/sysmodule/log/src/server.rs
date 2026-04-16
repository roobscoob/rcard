use ipc::Meta;
use once_cell::GlobalState;
use rcard_log::formatter::tags::TAG_END_OF_STREAM;
use rcard_log::{LogLevel, LogMetadata};
use sysmodule_log_api::LogError;

use crate::ringbuf::LogRing;
use crate::{usart_write, Reactor};
use generated::notifications;

fn get_timestamp() -> u64 {
    userlib::sys_get_timer().now
}

fn notify_logs(level: LogLevel) {
    let priority = match level {
        LogLevel::Panic => 5,
        LogLevel::Error => 4,
        LogLevel::Warn => 3,
        LogLevel::Info => 2,
        LogLevel::Debug => 1,
        LogLevel::Trace => 0,
    };
    let _ = Reactor::refresh(
        notifications::GROUP_ID_LOGS,
        0,
        priority,
        sysmodule_reactor_api::OverflowStrategy::DropOldest,
    );
}

// --- Framing protocol ---
//
// Wire format: COBS-encoded frames separated by 0x00 delimiters.
// Each COBS frame decodes to: [u64 stream_id LE][u8 length][data...]
// Each log message gets a unique stream_id. Within a stream:
// - First bytes: zerocopy-serialized LogMetadata
// - Remaining bytes: Format-encoded argument data
//
// COBS (Consistent Overhead Byte Stuffing) ensures 0x00 never appears
// in the encoded data, so the host can always resynchronize by scanning
// for a 0x00 delimiter.

/// Maximum size for inline log messages in the ring buffer.
const MAX_INLINE_LOG_SIZE: usize = 128;

/// Max raw chunk: 1 (type) + 8 (stream_id) + 1 (length) + 255 (data) = 265 bytes.
const MAX_RAW_CHUNK: usize = 1 + 9 + 255;
const MAX_ENCODED_CHUNK: usize = cobs::max_encoding_length(MAX_RAW_CHUNK) + 1;

// Scratch buffers parked in BSS. `send_frame` is on the hot dispatch
// path from `LogDispatcher::dispatch` → `LogResource::{log,start,write}`,
// so keeping these ~540 bytes off the stack matters for the tight
// sysmodule_log stack budget.
static FRAME_RAW: GlobalState<[u8; MAX_RAW_CHUNK]> =
    GlobalState::new([0u8; MAX_RAW_CHUNK]);
static FRAME_ENCODED: GlobalState<[u8; MAX_ENCODED_CHUNK]> =
    GlobalState::new([0u8; MAX_ENCODED_CHUNK]);

/// Send a COBS-framed log-fragment chunk over USART.
///
/// Wire layout after COBS-decoding + before the 0x00 delimiter:
/// ```text
///   [type = TYPE_LOG_FRAGMENT][stream_id: u64 LE][length: u8][data: length bytes]
/// ```
/// The type byte distinguishes log fragments from IPC-reply / tunnel-error
/// chunks on the same USART.
fn send_frame(stream_id: u64, data: &[u8]) {
    let mut offset = 0;
    while offset < data.len() {
        let chunk_len = (data.len() - offset).min(255);
        let raw_len = 1 + 9 + chunk_len;

        FRAME_RAW.with(|raw| {
            raw[0] = rcard_log::wire::TYPE_LOG_FRAGMENT;
            raw[1..9].copy_from_slice(&stream_id.to_le_bytes());
            raw[9] = chunk_len as u8;
            raw[10..raw_len].copy_from_slice(&data[offset..offset + chunk_len]);

            FRAME_ENCODED.with(|encoded| {
                let enc_len = cobs::encode(&raw[..raw_len], encoded);
                encoded[enc_len] = 0x00;
                usart_write(&encoded[..enc_len + 1]);
            });
        });

        offset += chunk_len;
    }
}

// --- Log state ---

struct LogState {
    ring: LogRing,
    next_stream_id: u64,
    next_log_id: u64,
}

impl LogState {
    const fn new() -> Self {
        Self {
            ring: LogRing::new(),
            next_stream_id: 1,
            next_log_id: 1,
        }
    }

    fn alloc_stream_id(&mut self) -> u64 {
        let id = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        id
    }

    fn alloc_log_id(&mut self) -> u64 {
        let id = self.next_log_id;
        self.next_log_id = self.next_log_id.wrapping_add(1);
        id
    }
}

static LOG_STATE: GlobalState<LogState> = GlobalState::new(LogState::new());

/// Shared method scratch buffer. Used by `LogResource::log` (metadata +
/// inline payload) and `LogResource::write` (streaming chunk). The
/// ipc::server dispatch is sequential so these methods never overlap,
/// and parking the buffer in BSS cuts ~150 bytes off `LogDispatcher::dispatch`'s
/// stack frame.
// +1 byte of headroom so `Log::log` can append TAG_END_OF_STREAM after a
// fully-sized inline payload without truncating caller data.
const METHOD_BUF_SIZE: usize = core::mem::size_of::<LogMetadata>() + MAX_INLINE_LOG_SIZE + 1;
static METHOD_BUF: GlobalState<[u8; METHOD_BUF_SIZE]> =
    GlobalState::new([0u8; METHOD_BUF_SIZE]);

// --- LogResource ---

pub struct LogResource {
    metadata: LogMetadata,
    stream_id: u64,
    idx: u8,
}

impl sysmodule_log_api::Log for LogResource {
    fn log(
        meta: Meta,
        level: LogLevel,
        species: u64,
        data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) {
        let task_index = meta.sender.task_index() as usize;
        let generation = meta.sender.generation().as_u8() as u16;

        LOG_STATE.with(|s| {
            let stream_id = s.alloc_stream_id();
            let log_id = s.alloc_log_id();

            let metadata = LogMetadata {
                level,
                timestamp: get_timestamp(),
                source: task_index as u16,
                generation,
                log_id,
                log_species: species,
            };

            const META_SIZE: usize = core::mem::size_of::<LogMetadata>();
            METHOD_BUF.with(|buf| {
                let meta_bytes = zerocopy::IntoBytes::as_bytes(&metadata);
                buf[..META_SIZE].copy_from_slice(meta_bytes);
                // Reserve 1 byte for the appended TAG_END_OF_STREAM terminator.
                let data_len = data.len().min(buf.len() - META_SIZE - 1);
                let _ = data.read_range(0, &mut buf[META_SIZE..META_SIZE + data_len]);
                let end = META_SIZE + data_len;
                buf[end] = TAG_END_OF_STREAM;
                send_frame(stream_id, &buf[..end + 1]);
                s.ring.push(log_id, 0, &buf[..end + 1]);
            });
        });
        notify_logs(level);
    }

    fn start(meta: Meta, level: LogLevel, species: u64) -> Option<Self> {
        let task_index = meta.sender.task_index() as usize;
        let generation = meta.sender.generation().as_u8() as u16;

        LOG_STATE
            .with(|s| {
                let stream_id = s.alloc_stream_id();
                let log_id = s.alloc_log_id();

                let metadata = LogMetadata {
                    level,
                    timestamp: get_timestamp(),
                    source: task_index as u16,
                    generation,
                    log_id,
                    log_species: species,
                };

                // Send metadata frame over USART (data arrives via write())
                let meta_bytes = zerocopy::IntoBytes::as_bytes(&metadata);
                send_frame(stream_id, meta_bytes);

                // Push fragment 0 with LogMetadata
                s.ring.push(log_id, 0, meta_bytes);

                Some(LogResource {
                    metadata,
                    stream_id,
                    idx: 1,
                })
            })
            .unwrap()
    }

    fn write(&mut self, _meta: Meta, data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>) {
        METHOD_BUF.with(|buf| {
            let len = data.len().min(MAX_INLINE_LOG_SIZE);
            let _ = data.read_range(0, &mut buf[..len]);
            send_frame(self.stream_id, &buf[..len]);
            LOG_STATE.with(|s| {
                s.ring.push(self.metadata.log_id, self.idx, &buf[..len]);
                self.idx = self.idx.saturating_add(1);
            });
        });
    }

    fn consume_since(
        meta: Meta,
        since_id: u64,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<u32, LogError> {
        let caller = meta.sender.task_index();
        if !notifications::LOGS_SUBSCRIBERS.contains(&caller) {
            return Err(LogError::Unauthorized);
        }

        const HEADER_SIZE: usize = 10;
        let buf_len = buf.len();

        let offset = LOG_STATE
            .with(|s| {
                let mut offset = 0usize;

                for chunk in s.ring.iter_since(since_id) {
                    let data_len = chunk.data.0.len() + chunk.data.1.len();
                    let entry_size = HEADER_SIZE + data_len;
                    if offset + entry_size > buf_len {
                        break;
                    }

                    let id_bytes = chunk.id.to_le_bytes();
                    let header = [
                        id_bytes[0],
                        id_bytes[1],
                        id_bytes[2],
                        id_bytes[3],
                        id_bytes[4],
                        id_bytes[5],
                        id_bytes[6],
                        id_bytes[7],
                        data_len as u8,
                        chunk.idx,
                    ];
                    let _ = buf.write_range(offset, &header);
                    offset += HEADER_SIZE;

                    if !chunk.data.0.is_empty() {
                        let _ = buf.write_range(offset, chunk.data.0);
                        offset += chunk.data.0.len();
                    }
                    if !chunk.data.1.is_empty() {
                        let _ = buf.write_range(offset, chunk.data.1);
                        offset += chunk.data.1.len();
                    }
                }

                offset
            })
            .unwrap();

        Ok(offset as u32)
    }
}

impl Drop for LogResource {
    fn drop(&mut self) {
        // Terminate the stream on the wire and in the ring. The producer
        // dropping its IPC handle is the "no more data" signal; converting
        // that into TAG_END_OF_STREAM here is what lets the host decoder
        // evict the stream, even when the producer aborted mid-format.
        send_frame(self.stream_id, &[TAG_END_OF_STREAM]);
        LOG_STATE.with(|s| {
            s.ring.push(self.metadata.log_id, self.idx, &[TAG_END_OF_STREAM]);
        });
    }
}
