//! COBS-framed ring buffer writer for raw block storage.
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

use storage_api::StorageDyn;

const BLOCK_SIZE: u32 = 512;

/// A streaming ring buffer writer.
///
/// Call [`begin`] to start a message, [`write`] to append data,
/// and [`end`] to finalize. Messages can be arbitrarily large.
pub struct RingWriter {
    storage: StorageDyn,
    pos: u32,
    capacity: u32,
    counter: u32,
    // Block I/O cache (one sector at a time)
    block_buf: [u8; BLOCK_SIZE as usize],
    block_num: u32,
    block_dirty: bool,
    block_loaded: bool,
    // COBS encoding state
    cobs_buf: [u8; 254],
    cobs_len: u8,
    in_message: bool,
}

impl RingWriter {
    /// Create a new ring writer from a storage handle.
    pub fn new(storage: StorageDyn) -> Self {
        let capacity = storage.block_count().unwrap_or(0) * BLOCK_SIZE;
        Self {
            storage,
            pos: 0,
            capacity,
            counter: 0,
            block_buf: [0u8; BLOCK_SIZE as usize],
            block_num: u32::MAX,
            block_dirty: false,
            block_loaded: false,
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
        self.flush_block();

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

    // ── Block-buffered byte output ──────────────────────────────────

    fn emit_byte(&mut self, b: u8) {
        let block = self.pos / BLOCK_SIZE;
        let offset = (self.pos % BLOCK_SIZE) as usize;

        if !self.block_loaded || self.block_num != block {
            self.flush_block();
            let _ = self.storage.read_block(block, &mut self.block_buf);
            self.block_num = block;
            self.block_loaded = true;
        }

        self.block_buf[offset] = b;
        self.block_dirty = true;
        self.pos = (self.pos + 1) % self.capacity;
    }

    fn flush_block(&mut self) {
        if self.block_dirty && self.block_loaded {
            let _ = self.storage.write_block(self.block_num, &self.block_buf);
            self.block_dirty = false;
        }
    }
}
