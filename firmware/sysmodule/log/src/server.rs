use hubpack::SerializedSize;
use ipc::Meta;
use once_cell::GlobalState;
use rcard_log::{LogLevel, LogMetadata};
use sysmodule_log_api::LogError;

use crate::ringbuf::{pack_time, LogRing};
use crate::{generated, usart_write, Reactor, Time};

fn get_packed_time() -> u64 {
    Time::get_time()
        .ok()
        .flatten()
        .map(|dt| pack_time(&dt))
        .unwrap_or(0)
}

fn get_timestamp() -> u64 {
    get_packed_time()
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
        generated::GROUP_ID_LOGS,
        0,
        priority,
        sysmodule_reactor_api::OverflowStrategy::DropOldest,
    );
}

// --- Framing protocol ---
//
// Wire format per frame: [u64 stream_id LE][u8 length][data...]
// Each log message gets a unique stream_id. Within a stream:
// - First bytes: hubpack-serialized LogMetadata
// - Remaining bytes: Format-encoded argument data

/// Maximum size for inline log messages in the ring buffer.
const MAX_INLINE_LOG_SIZE: usize = 128;

/// Send a frame over USART: [u64 stream_id LE][u8 length][data...]
fn send_frame(stream_id: u64, data: &[u8]) {
    let mut offset = 0;
    while offset < data.len() {
        let chunk_len = (data.len() - offset).min(255);
        usart_write(&stream_id.to_le_bytes());
        usart_write(&[chunk_len as u8]);
        usart_write(&data[offset..offset + chunk_len]);
        offset += chunk_len;
    }
}

/// Build and send LogMetadata + data as framed output.
fn send_log_framed(
    stream_id: u64,
    level: LogLevel,
    species: u64,
    source: u16,
    generation: u16,
    log_id: u64,
    data_fn: impl FnOnce(&mut [u8]) -> usize,
) {
    let metadata = LogMetadata {
        level,
        timestamp: get_timestamp(),
        source,
        generation,
        log_id,
        log_species: species,
    };

    let mut buf = [0u8; LogMetadata::MAX_SIZE + MAX_INLINE_LOG_SIZE];
    let meta_len = hubpack::serialize(&mut buf, &metadata).unwrap_or(0);
    let data_len = data_fn(&mut buf[meta_len..]);

    send_frame(stream_id, &buf[..meta_len + data_len]);
}

// --- Log state ---

struct LogState {
    ring: LogRing,
    next_stream_id: u64,
}

impl LogState {
    const fn new() -> Self {
        Self {
            ring: LogRing::new(),
            next_stream_id: 1,
        }
    }

    fn alloc_stream_id(&mut self) -> u64 {
        let id = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        id
    }
}

static LOG_STATE: GlobalState<LogState> = GlobalState::new(LogState::new());

// --- LogResource ---

pub struct LogResource {
    ring_id: u32,
    stream_id: u64,
    level: LogLevel,
    idx: usize,
    task_index: usize,
}

impl sysmodule_log_api::Log for LogResource {
    fn log(
        meta: Meta,
        level: LogLevel,
        species: u64,
        data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) {
        let task_index = meta.sender.task_index() as usize;
        let generation = 0u16;

        LOG_STATE.with(|s| {
            let stream_id = s.alloc_stream_id();
            let ring_id = s.ring.alloc_id();

            send_log_framed(
                stream_id,
                level,
                species,
                task_index as u16,
                generation,
                ring_id as u64,
                |buf| {
                    let len = data.len().min(buf.len());
                    let _ = data.read_range(0, &mut buf[..len]);
                    len
                },
            );

            let mut ring_buf = [0u8; MAX_INLINE_LOG_SIZE];
            let len = data.len().min(ring_buf.len());
            let _ = data.read_range(0, &mut ring_buf[..len]);
            let time = get_packed_time();
            s.ring.push(ring_id, level, task_index as u16, &ring_buf[..len], 0, time);
        });
        notify_logs(level);
    }

    fn start(meta: Meta, level: LogLevel, species: u64) -> Option<Self> {
        let task_index = meta.sender.task_index() as usize;
        let generation = 0u16;

        LOG_STATE.with(|s| {
            let stream_id = s.alloc_stream_id();
            let ring_id = s.ring.alloc_id();

            // Send metadata frame (data arrives via write())
            send_log_framed(
                stream_id,
                level,
                species,
                task_index as u16,
                generation,
                ring_id as u64,
                |_| 0,
            );

            Some(LogResource {
                ring_id,
                stream_id,
                level,
                idx: 0,
                task_index,
            })
        })
    }

    fn write(
        &mut self,
        _meta: Meta,
        data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) {
        let mut buf = [0u8; MAX_INLINE_LOG_SIZE];
        let len = data.len().min(buf.len());
        let _ = data.read_range(0, &mut buf[..len]);
        send_frame(self.stream_id, &buf[..len]);

        LOG_STATE.with(|s| {
            let time = get_packed_time();
            s.ring.push(
                self.ring_id,
                self.level,
                self.task_index as u16,
                &buf[..len],
                self.idx,
                time,
            );
            self.idx += 1;
        });
    }

    fn consume_since(
        meta: Meta,
        since_id: u32,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<u32, LogError> {
        let caller = meta.sender.task_index();
        if !generated::LOGS_SUBSCRIBERS.contains(&caller) {
            return Err(LogError::Unauthorized);
        }

        const HEADER_SIZE: usize = 19;
        let buf_len = buf.len();

        let offset = LOG_STATE.with(|s| {
            let mut offset = 0usize;

            for chunk in s.ring.iter_since(since_id) {
                let data_len = chunk.data.0.len() + chunk.data.1.len();
                let entry_size = HEADER_SIZE + data_len;
                if offset + entry_size > buf_len {
                    break;
                }

                let time_bytes = chunk.time.to_le_bytes();
                let header = [
                    (chunk.id & 0xFF) as u8,
                    ((chunk.id >> 8) & 0xFF) as u8,
                    ((chunk.id >> 16) & 0xFF) as u8,
                    ((chunk.id >> 24) & 0xFF) as u8,
                    chunk.level as u8,
                    (chunk.task & 0xFF) as u8,
                    ((chunk.task >> 8) & 0xFF) as u8,
                    (chunk.idx & 0xFF) as u8,
                    ((chunk.idx >> 8) & 0xFF) as u8,
                    time_bytes[0],
                    time_bytes[1],
                    time_bytes[2],
                    time_bytes[3],
                    time_bytes[4],
                    time_bytes[5],
                    time_bytes[6],
                    time_bytes[7],
                    (data_len & 0xFF) as u8,
                    ((data_len >> 8) & 0xFF) as u8,
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
        });

        Ok(offset as u32)
    }
}

impl Drop for LogResource {
    fn drop(&mut self) {
        // No action needed — stream stays in the emulator-side HashMap.
    }
}
