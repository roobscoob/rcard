//! COBS-framed ring buffer writer for raw byte-addressed storage.
//!
//! Message format on disk:
//!
//! ```text
//! \x00  [COBS(counter:u32le ++ data)]  \x00
//! ```
//!
//! - Each message is bookended by null bytes.
//! - The payload (4-byte little-endian counter + arbitrary data) is
//!   COBS-encoded, guaranteeing no internal null bytes.
//! - The counter increments monotonically (wrapping at u32::MAX).
//! - The write head moves left-to-right, wrapping around the partition.
//! - To find the oldest valid message, locate the counter discontinuity
//!   (the "head"), then scan forward for the first null-pair boundary.
//!
//! Uses `erase()` + `program()` directly for sequential NOR flash writes.

use storage_api::StorageDyn;

/// Maximum program page size we support (compile-time buffer).
const MAX_PROGRAM_SIZE: usize = 256;

/// A streaming ring buffer writer.
///
/// Call [`begin`] to start a message, [`write`] to append data,
/// and [`end`] to finalize. Messages can be arbitrarily large.
pub struct RingWriter {
    storage: StorageDyn,
    /// Current write position (byte offset into the partition).
    pos: u32,
    /// Total partition size in bytes.
    capacity: u32,
    /// Erase unit size in bytes.
    erase_size: u32,
    /// Program unit size in bytes.
    program_size: u32,
    /// Monotonic message counter.
    counter: u32,
    // Program buffer — accumulates bytes until a full page is ready.
    prog_buf: [u8; MAX_PROGRAM_SIZE],
    /// Byte offset where the current prog_buf starts.
    prog_buf_start: u32,
    /// Number of valid bytes in prog_buf.
    prog_buf_len: u16,
    // COBS encoding state
    cobs_buf: [u8; 254],
    cobs_len: u8,
    in_message: bool,
}

impl RingWriter {
    /// Create a new ring writer from a storage handle.
    pub fn new(storage: StorageDyn) -> Self {
        let geom = storage.geometry().unwrap_or(storage_api::Geometry {
            total_size: 0,
            erase_size: 4096,
            program_size: 256,
            read_size: 1,
        });
        assert!(geom.program_size as usize <= MAX_PROGRAM_SIZE);
        Self {
            storage,
            pos: 0,
            capacity: geom.total_size,
            erase_size: geom.erase_size,
            program_size: geom.program_size,
            counter: 0,
            prog_buf: [0u8; MAX_PROGRAM_SIZE],
            prog_buf_start: 0,
            prog_buf_len: 0,
            cobs_buf: [0u8; 254],
            cobs_len: 0,
            in_message: false,
        }
    }

    /// Start a new message. Writes the leading `\x00` delimiter and
    /// COBS-encodes the 4-byte counter.
    pub fn begin(&mut self) {
        if self.in_message {
            self.end();
        }
        self.in_message = true;
        self.cobs_len = 0;

        // Leading null delimiter.
        self.emit_byte(0x00);

        // COBS-encode the counter (u32 little-endian).
        let c = self.counter.to_le_bytes();
        for &b in &c {
            self.cobs_feed(b);
        }
    }

    /// Append data to the current message. May be called multiple times
    /// between [`begin`] and [`end`].
    pub fn write(&mut self, data: &[u8]) {
        for &b in data {
            self.cobs_feed(b);
        }
    }

    /// Finalize the current message. Flushes the COBS encoder and writes
    /// the trailing `\x00` delimiter.
    pub fn end(&mut self) {
        if !self.in_message {
            return;
        }
        self.cobs_flush();
        self.emit_byte(0x00);
        self.flush_prog();

        self.counter = self.counter.wrapping_add(1);
        self.in_message = false;
    }

    /// Current write position (byte offset into the partition).
    pub fn position(&self) -> u32 {
        self.pos
    }

    /// Current counter value (next message will use this).
    pub fn counter(&self) -> u32 {
        self.counter
    }

    // ── COBS encoder ────────────────────────────────────────────────

    fn cobs_feed(&mut self, b: u8) {
        if b == 0 {
            // Zero byte terminates this group.
            self.cobs_flush();
        } else {
            self.cobs_buf[self.cobs_len as usize] = b;
            self.cobs_len += 1;
            if self.cobs_len == 254 {
                // Full group with no zero — code byte is 0xFF.
                self.cobs_flush_full();
            }
        }
    }

    /// Flush a COBS group ended by a zero or end-of-message.
    /// Code byte = `len + 1` (distance to the implicit zero).
    fn cobs_flush(&mut self) {
        let code = self.cobs_len + 1;
        self.emit_byte(code);
        for i in 0..self.cobs_len as usize {
            self.emit_byte(self.cobs_buf[i]);
        }
        self.cobs_len = 0;
    }

    /// Flush a full 254-byte group (no implicit trailing zero).
    /// Code byte = 0xFF.
    fn cobs_flush_full(&mut self) {
        self.emit_byte(0xFF);
        for i in 0..254 {
            self.emit_byte(self.cobs_buf[i]);
        }
        self.cobs_len = 0;
    }

    // ── Program-buffered byte output ───────────────────────────────

    fn emit_byte(&mut self, b: u8) {
        // If entering a new erase sector, erase it first.
        if self.pos % self.erase_size == 0 && self.prog_buf_len == 0 {
            let _ = self.storage.erase(self.pos, self.erase_size);
        }

        // Start a new prog buffer if empty.
        if self.prog_buf_len == 0 {
            self.prog_buf_start = self.pos;
        }

        self.prog_buf[self.prog_buf_len as usize] = b;
        self.prog_buf_len += 1;
        self.pos = (self.pos + 1) % self.capacity;

        // Flush when the program buffer is full.
        if self.prog_buf_len as u32 == self.program_size {
            self.flush_prog();
        }
    }

    fn flush_prog(&mut self) {
        if self.prog_buf_len == 0 {
            return;
        }
        // Pad remaining bytes with 0x00 (null = COBS delimiter, safe).
        for i in self.prog_buf_len as usize..self.program_size as usize {
            self.prog_buf[i] = 0x00;
        }
        let _ = self
            .storage
            .program(self.prog_buf_start, &self.prog_buf[..self.program_size as usize]);
        self.prog_buf_len = 0;
    }
}
