use crate::formatter::Writer;
use crate::LogLevel;

const BUF_SIZE: usize = 128;

extern "Rust" {
    /// Send a complete log message in one shot (small messages).
    fn __rcard_log_send(level: u8, species: u64, data: &[u8]);

    /// Start a streaming log session (large messages).
    /// Returns `None` if the log is rejected — all subsequent writes are no-ops.
    fn __rcard_log_start(level: u8, species: u64) -> Option<u64>;

    /// Write a chunk to an active streaming session.
    fn __rcard_log_write(handle: u64, data: &[u8]);

    /// End a streaming session (drops the handle on the server).
    fn __rcard_log_end(handle: u64);
}

/// State of the streaming session.
enum Session {
    /// Haven't needed streaming yet (message still fits in buffer).
    Buffered,
    /// Streaming session active with this handle.
    Active(u64),
    /// Start was rejected — discard everything.
    Rejected,
}

/// A buffering writer that sends log data via extern "Rust" fns.
///
/// - Small messages (fit in buffer): flushed on drop via `__rcard_log_send`.
/// - Large messages (overflow buffer): lazily upgraded to streaming via
///   `__rcard_log_start` + `__rcard_log_write`, remainder flushed on drop.
/// - If `__rcard_log_start` returns `None`, the log is silently dropped.
pub struct LogWriter {
    level: u8,
    species: u64,
    buf: [u8; BUF_SIZE],
    pos: usize,
    session: Session,
}

impl LogWriter {
    #[inline]
    pub fn new(level: LogLevel, species: u64) -> Self {
        LogWriter {
            level: level as u8,
            species,
            buf: [0u8; BUF_SIZE],
            pos: 0,
            session: Session::Buffered,
        }
    }

    fn flush_buf(&mut self) {
        if self.pos == 0 {
            return;
        }

        let handle = match self.session {
            Session::Active(h) => h,
            Session::Rejected => {
                self.pos = 0;
                return;
            }
            Session::Buffered => match unsafe { __rcard_log_start(self.level, self.species) } {
                Some(h) => {
                    self.session = Session::Active(h);
                    h
                }
                None => {
                    self.session = Session::Rejected;
                    self.pos = 0;
                    return;
                }
            },
        };

        unsafe {
            __rcard_log_write(handle, &self.buf[..self.pos]);
        }
        self.pos = 0;
    }
}

impl Writer for LogWriter {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        if matches!(self.session, Session::Rejected) {
            return;
        }

        let mut offset = 0;
        while offset < bytes.len() {
            let remaining_buf = BUF_SIZE - self.pos;
            let chunk = bytes.len() - offset;

            if chunk <= remaining_buf {
                // Fits in buffer
                self.buf[self.pos..self.pos + chunk]
                    .copy_from_slice(&bytes[offset..offset + chunk]);
                self.pos += chunk;
                offset += chunk;
            } else {
                // Fill buffer, flush, continue
                self.buf[self.pos..BUF_SIZE]
                    .copy_from_slice(&bytes[offset..offset + remaining_buf]);
                self.pos = BUF_SIZE;
                self.flush_buf();
                offset += remaining_buf;

                // flush_buf may have transitioned to Rejected
                if matches!(self.session, Session::Rejected) {
                    return;
                }
            }
        }
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        match self.session {
            Session::Rejected => {}
            Session::Active(h) => {
                // Flush remainder, then end session
                if self.pos > 0 {
                    self.flush_buf();
                }
                unsafe {
                    __rcard_log_end(h);
                }
            }
            Session::Buffered => {
                // Single-shot send (even empty data is meaningful:
                // it registers the log occurrence with its level and species)
                unsafe {
                    __rcard_log_send(self.level, self.species, &self.buf[..self.pos]);
                }
            }
        }
    }
}
