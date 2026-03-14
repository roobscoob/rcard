use ipc::Meta;
use once_cell::GlobalState;
use rcard_log::{LogLevel, LogMetadata};
use sysmodule_log_api::LogError;

use crate::ringbuf::LogRing;
use crate::{generated, usart_write, Reactor};

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

        LOG_STATE.with(|s| {
            let stream_id = s.alloc_stream_id();
            let log_id = s.alloc_log_id();

            let metadata = LogMetadata {
                level,
                timestamp: get_timestamp(),
                source: task_index as u16,
                generation: 0,
                log_id,
                log_species: species,
            };

            // Send over USART
            const META_SIZE: usize = core::mem::size_of::<LogMetadata>();
            let mut buf = [0u8; META_SIZE + MAX_INLINE_LOG_SIZE];
            let meta_bytes = zerocopy::IntoBytes::as_bytes(&metadata);
            buf[..META_SIZE].copy_from_slice(meta_bytes);
            let meta_len = META_SIZE;
            let data_len = data.len().min(buf.len() - meta_len);
            let _ = data.read_range(0, &mut buf[meta_len..meta_len + data_len]);
            send_frame(stream_id, &buf[..meta_len + data_len]);

            // Push to ring
            s.ring.push(log_id, 0, &buf[..meta_len + data_len]);
        });
        notify_logs(level);
    }

    fn start(meta: Meta, level: LogLevel, species: u64) -> Option<Self> {
        let task_index = meta.sender.task_index() as usize;

        LOG_STATE
            .with(|s| {
                let stream_id = s.alloc_stream_id();
                let log_id = s.alloc_log_id();

                let metadata = LogMetadata {
                    level,
                    timestamp: get_timestamp(),
                    source: task_index as u16,
                    generation: 0,
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
        let mut buf = [0u8; MAX_INLINE_LOG_SIZE];
        let len = data.len().min(buf.len());
        let _ = data.read_range(0, &mut buf[..len]);
        send_frame(self.stream_id, &buf[..len]);

        LOG_STATE.with(|s| {
            s.ring.push(self.metadata.log_id, self.idx, &buf[..len]);
            self.idx = self.idx.saturating_add(1);
        });
    }

    fn consume_since(
        meta: Meta,
        since_id: u64,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<u32, LogError> {
        let caller = meta.sender.task_index();
        if !generated::LOGS_SUBSCRIBERS.contains(&caller) {
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
        // No action needed — stream stays in the emulator-side HashMap.
    }
}
