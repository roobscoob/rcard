use ipc::Meta;
use once_cell::GlobalState;
use sysmodule_log_api::*;

use crate::fmt::{level_str, read_leased_chunks, write_prefix_to, write_tag, write_timestamp};
use crate::ringbuf::{pack_time, LogRing};
use crate::{generated, usart_write, Reactor, Time};

fn get_packed_time() -> u64 {
    Time::get_time()
        .ok()
        .flatten()
        .map(|dt| pack_time(&dt))
        .unwrap_or(0)
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

// --- Log state ---

/// Maximum size for inline log messages (Log::log without a Log::start/write
/// sequence). Messages longer than this are silently truncated in the ring
/// buffer; USART output is unaffected.
const MAX_INLINE_LOG_SIZE: usize = 128;

const MAX_WRITER_DEPTH: usize = 16;

#[derive(Clone, Copy)]
#[allow(dead_code)]
struct WriterEntry {
    id: u32,
    task_index: usize,
    level: LogLevel,
}

struct InterruptTracker {
    stack: [WriterEntry; MAX_WRITER_DEPTH],
    depth: usize,
    needs_cont: bool,
}

impl InterruptTracker {
    const fn new() -> Self {
        const EMPTY: WriterEntry = WriterEntry {
            id: 0,
            task_index: 0,
            level: LogLevel::Trace,
        };
        Self {
            stack: [EMPTY; MAX_WRITER_DEPTH],
            depth: 0,
            needs_cont: false,
        }
    }
}

struct LogState {
    ring: LogRing,
    tracker: InterruptTracker,
}

impl LogState {
    const fn new() -> Self {
        Self {
            ring: LogRing::new(),
            tracker: InterruptTracker::new(),
        }
    }
}

static LOG_STATE: GlobalState<LogState> = GlobalState::new(LogState::new());

/// Write an indented prefix: "TIMESTAMP     ↳ [LEVEL task] "
fn write_indented_prefix(level: LogLevel, task_name: &str, depth: usize) {
    write_timestamp(|d| usart_write(d));
    for _ in 1..depth {
        usart_write(b"    ");
    }
    usart_write(b"\xe2\x86\xb3 "); // "↳ "
    write_tag(level, task_name, |d| usart_write(d));
}

/// Write a plain indented prefix (no ↳): "TIMESTAMP       [LEVEL task] "
fn write_plain_indented_prefix(level: LogLevel, task_name: &str, depth: usize) {
    write_timestamp(|d| usart_write(d));
    for _ in 1..depth {
        usart_write(b"    ");
    }
    usart_write(b"  "); // align with where ↳ would be
    write_tag(level, task_name, |d| usart_write(d));
}

/// Compute the display width of a prefix: timestamp + indent + tag
fn prefix_pad(level: LogLevel, task_name: &str, depth: usize) -> usize {
    // Timestamp: "DD/MM/YY HH:MM:SS " = 18 chars
    let ts = 18;
    // Tag: "[LEVEL task] " = level_str.len() + task_name.len() + 4
    let tag = level_str(level).len() + task_name.len() + 4;
    // Indent: (depth-1)*4 + 2 for depth >= 2, 0 for depth <= 1
    let indent = if depth > 1 { (depth - 1) * 4 + 2 } else { 0 };
    ts + indent + tag
}

/// Write data to USART, replacing \n with \r\n + padding spaces.
fn usart_write_padded(data: &[u8], pad: usize) {
    static SPACES: [u8; 64] = [b' '; 64];
    let mut start = 0;
    for i in 0..data.len() {
        if data[i] == b'\n' {
            if i > start {
                usart_write(&data[start..i]);
            }
            usart_write(b"\r\n");
            let mut rem = pad;
            while rem > 0 {
                let n = rem.min(SPACES.len());
                usart_write(&SPACES[..n]);
                rem -= n;
            }
            start = i + 1;
        }
    }
    if start < data.len() {
        usart_write(&data[start..]);
    }
}

// --- LogResource ---

pub struct LogResource {
    id: u32,
    level: LogLevel,
    idx: usize,
    task_index: usize,
    pad: usize,
}

impl Log for LogResource {
    fn log(meta: Meta, level: LogLevel, data: idyll_runtime::Leased<idyll_runtime::Read, u8>) {
        let task_index = meta.sender.task_index() as usize;
        let task_name = generated::TASK_NAMES.get(task_index).unwrap_or(&"???");

        LOG_STATE.with(|s| {
            // Handle interrupt of active writer
            let t = &mut s.tracker;
            let depth = if t.depth > 0 {
                let d = t.depth + 1;
                if !t.needs_cont {
                    usart_write(b" [INTR]\r\n");
                    t.needs_cont = true;
                    write_indented_prefix(level, task_name, d);
                } else {
                    write_plain_indented_prefix(level, task_name, d);
                }
                d
            } else {
                write_prefix_to(level, task_name, |d| usart_write(d));
                0
            };
            let pad = prefix_pad(level, task_name, depth);
            read_leased_chunks(&data, |chunk| usart_write_padded(chunk, pad));
            usart_write(b"\r\n");

            // Push single entry to ring buffer
            let id = s.ring.alloc_id();
            let mut buf = [0u8; MAX_INLINE_LOG_SIZE];
            let len = data.len().min(buf.len());
            let _ = data.read_range(0, &mut buf[..len]);
            let time = get_packed_time();
            s.ring.push(id, level, task_index as u16, &buf[..len], 0, time);
        });
        notify_logs(level);
    }

    fn start(meta: Meta, level: LogLevel) -> Option<Self> {
        let task_index = meta.sender.task_index() as usize;
        let task_name = generated::TASK_NAMES.get(task_index).unwrap_or(&"???");

        LOG_STATE.with(|s| {
            let t = &mut s.tracker;

            // Handle interrupt of active writer
            let was_active = t.depth > 0;
            let was_mid_line = was_active && !t.needs_cont;
            if was_mid_line {
                usart_write(b" [INTR]\r\n");
            }

            // Push onto writer stack
            if t.depth < MAX_WRITER_DEPTH {
                t.stack[t.depth] = WriterEntry {
                    id: 0, // filled below
                    task_index,
                    level,
                };
                t.depth += 1;
            }
            t.needs_cont = false;

            // Write prefix
            let depth = t.depth;
            if was_mid_line {
                write_indented_prefix(level, task_name, depth);
            } else if was_active {
                write_plain_indented_prefix(level, task_name, depth);
            } else {
                write_prefix_to(level, task_name, |d| usart_write(d));
            }
            let pad = prefix_pad(level, task_name, depth);

            let id = s.ring.alloc_id();

            // Update the stack entry with the allocated id
            if t.depth > 0 {
                t.stack[t.depth - 1].id = id;
            }

            Some(LogResource {
                id,
                level,
                idx: 0,
                task_index,
                pad,
            })
        })
    }

    fn write(&mut self, _meta: Meta, data: idyll_runtime::Leased<idyll_runtime::Read, u8>) {
        LOG_STATE.with(|s| {
            // If this writer was interrupted and is now resuming, print CONT prefix
            let t = &mut s.tracker;
            if t.needs_cont && t.depth > 0 && t.stack[t.depth - 1].id == self.id {
                let task_name = generated::TASK_NAMES.get(self.task_index).unwrap_or(&"???");
                write_timestamp(|d| usart_write(d));
                for _ in 1..t.depth {
                    usart_write(b"    ");
                }
                if t.depth > 1 {
                    usart_write(b"  ");
                }
                usart_write(b"[CONT ");
                usart_write(task_name.as_bytes());
                usart_write(b"] ");
                t.needs_cont = false;
            }

            // Write to USART
            read_leased_chunks(&data, |chunk| usart_write_padded(chunk, self.pad));

            // Push entry to ring buffer
            let mut buf = [0u8; MAX_INLINE_LOG_SIZE];
            let len = data.len().min(buf.len());
            let _ = data.read_range(0, &mut buf[..len]);
            let time = get_packed_time();
            s.ring.push(self.id, self.level, self.task_index as u16, &buf[..len], self.idx, time);
            self.idx += 1;
        });
    }

    fn consume_since(
        meta: Meta,
        since_id: u32,
        buf: idyll_runtime::Leased<idyll_runtime::Write, u8>,
    ) -> Result<u32, sysmodule_log_api::LogError> {
        // Only allow tasks that subscribe to "logs" notifications.
        let caller = meta.sender.task_index();
        if !generated::LOGS_SUBSCRIBERS.contains(&caller) {
            return Err(sysmodule_log_api::LogError::Unauthorized);
        }

        // Entry wire format: [id: 4][level: 1][task: 2][idx: 2][time: 8][len: 2][data: len]
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
        usart_write(b"\r\n");

        LOG_STATE.with(|s| {
            let t = &mut s.tracker;
            // Pop from writer stack
            if t.depth > 0 && t.stack[t.depth - 1].id == self.id {
                t.depth -= 1;
                // The writer below (if any) will need a CONT prefix on next write
                t.needs_cont = t.depth > 0;
            } else {
                // Dropped out of order — find and remove from stack
                for i in 0..t.depth {
                    if t.stack[i].id == self.id {
                        // Shift entries down
                        for j in i..t.depth - 1 {
                            t.stack[j] = t.stack[j + 1];
                        }
                        t.depth -= 1;
                        break;
                    }
                }
            }
        });
    }
}
