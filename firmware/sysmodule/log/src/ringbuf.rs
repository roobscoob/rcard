use sysmodule_log_api::LogLevel;
use sysmodule_time_api::SystemDateTime;

const BUF_SIZE: usize = 4096;

// Frame layout: [id: 4][level: 1][task: 2][idx: 2][time: 8][len: 2][data: len]
const HEADER_SIZE: usize = 19;

pub struct LogRing {
    buf: [u8; BUF_SIZE],
    head: usize,
    used: usize,
    count: usize,
    next_id: u32,
}

pub struct LogChunk<'a> {
    pub id: u32,
    pub level: LogLevel,
    pub task: u16,
    pub idx: u16,
    pub time: u64,
    pub data: (&'a [u8], &'a [u8]),
}

pub fn pack_time(dt: &SystemDateTime) -> u64 {
    (dt.year as u64)
        | (dt.month as u64) << 16
        | (dt.day as u64) << 24
        | (dt.weekday as u64) << 32
        | (dt.hour as u64) << 40
        | (dt.minute as u64) << 48
        | (dt.second as u64) << 56
}

pub struct Iter<'a> {
    ring: &'a LogRing,
    offset: usize,
    remaining: usize,
}

impl<'a> Iterator for Iter<'a> {
    type Item = LogChunk<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        let mut hdr = [0u8; HEADER_SIZE];
        self.ring.read_at(self.offset, &mut hdr);

        let id = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let level = LogLevel::from_u8(hdr[4]);
        let task = u16::from_le_bytes([hdr[5], hdr[6]]);
        let idx = u16::from_le_bytes([hdr[7], hdr[8]]);
        let time = u64::from_le_bytes([hdr[9], hdr[10], hdr[11], hdr[12], hdr[13], hdr[14], hdr[15], hdr[16]]);
        let data_len = u16::from_le_bytes([hdr[17], hdr[18]]) as usize;

        let data_start = (self.offset + HEADER_SIZE) % BUF_SIZE;
        let data = if data_start + data_len <= BUF_SIZE {
            (&self.ring.buf[data_start..data_start + data_len], &[][..])
        } else {
            (
                &self.ring.buf[data_start..],
                &self.ring.buf[..data_len - (BUF_SIZE - data_start)],
            )
        };

        self.offset = (self.offset + HEADER_SIZE + data_len) % BUF_SIZE;
        self.remaining -= 1;

        Some(LogChunk {
            id,
            level,
            task,
            idx,
            time,
            data,
        })
    }
}

impl LogRing {
    pub const fn new() -> Self {
        Self {
            buf: [0u8; BUF_SIZE],
            head: 0,
            used: 0,
            count: 0,
            next_id: 1,
        }
    }

    fn read_at(&self, offset: usize, dst: &mut [u8]) {
        let start = offset % BUF_SIZE;
        let len = dst.len();
        let first = (BUF_SIZE - start).min(len);
        dst[..first].copy_from_slice(&self.buf[start..start + first]);
        if first < len {
            dst[first..].copy_from_slice(&self.buf[..len - first]);
        }
    }

    fn write_at(&mut self, offset: usize, src: &[u8]) {
        let start = offset % BUF_SIZE;
        let first = (BUF_SIZE - start).min(src.len());
        self.buf[start..start + first].copy_from_slice(&src[..first]);
        if first < src.len() {
            self.buf[..src.len() - first].copy_from_slice(&src[first..]);
        }
    }

    pub fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_mul(1664525).wrapping_add(1013904223);
        id
    }

    pub fn push(&mut self, id: u32, level: LogLevel, task: u16, data: &[u8], idx: usize, time: u64) {
        let frame_size = HEADER_SIZE + data.len();
        if frame_size > BUF_SIZE {
            return;
        }

        // Evict oldest frames until we have space
        while self.used + frame_size > BUF_SIZE {
            self.evict_oldest();
        }

        // Write header
        let write_pos = (self.head + self.used) % BUF_SIZE;
        let time_bytes = time.to_le_bytes();
        let hdr = [
            (id & 0xFF) as u8,
            ((id >> 8) & 0xFF) as u8,
            ((id >> 16) & 0xFF) as u8,
            ((id >> 24) & 0xFF) as u8,
            level as u8,
            (task & 0xFF) as u8,
            ((task >> 8) & 0xFF) as u8,
            (idx & 0xFF) as u8,
            ((idx >> 8) & 0xFF) as u8,
            time_bytes[0], time_bytes[1], time_bytes[2], time_bytes[3],
            time_bytes[4], time_bytes[5], time_bytes[6], time_bytes[7],
            (data.len() & 0xFF) as u8,
            ((data.len() >> 8) & 0xFF) as u8,
        ];
        self.write_at(write_pos, &hdr);
        self.write_at((write_pos + HEADER_SIZE) % BUF_SIZE, data);
        self.used += frame_size;
        self.count += 1;
    }

    fn evict_oldest(&mut self) {
        if self.count == 0 {
            return;
        }
        let mut hdr = [0u8; HEADER_SIZE];
        self.read_at(self.head, &mut hdr);
        let data_len = u16::from_le_bytes([hdr[17], hdr[18]]) as usize;
        let frame_size = HEADER_SIZE + data_len;
        self.head = (self.head + frame_size) % BUF_SIZE;
        self.used -= frame_size;
        self.count -= 1;
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn iter(&self) -> Iter<'_> {
        Iter {
            ring: self,
            offset: self.head,
            remaining: self.count,
        }
    }

    /// Iterate chunks after the last occurrence of `id`.
    /// If `id` is not found (evicted or 0), iterates everything.
    pub fn iter_since(&self, id: u32) -> Iter<'_> {
        if id == 0 {
            return self.iter();
        }

        let mut offset = self.head;
        let mut found_offset = None;
        let mut found_remaining = 0;

        for i in 0..self.count {
            let mut hdr = [0u8; HEADER_SIZE];
            self.read_at(offset, &mut hdr);
            let entry_id = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
            let data_len = u16::from_le_bytes([hdr[17], hdr[18]]) as usize;
            let frame_size = HEADER_SIZE + data_len;
            offset = (offset + frame_size) % BUF_SIZE;

            if entry_id == id {
                // Mark the position *after* this entry
                found_offset = Some(offset);
                found_remaining = self.count - i - 1;
            }
        }

        match found_offset {
            Some(off) => Iter {
                ring: self,
                offset: off,
                remaining: found_remaining,
            },
            None => self.iter(),
        }
    }
}

