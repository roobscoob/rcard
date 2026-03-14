const BUF_SIZE: usize = 4096;

// Frame layout: [id: 8][len: 1][idx: 1][data: len]
const HEADER_SIZE: usize = 10;

pub struct LogRing {
    buf: [u8; BUF_SIZE],
    head: usize,
    used: usize,
    count: usize,
}

pub struct LogChunk<'a> {
    pub id: u64,
    pub idx: u8,
    pub data: (&'a [u8], &'a [u8]),
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

        let id = u64::from_le_bytes([
            hdr[0], hdr[1], hdr[2], hdr[3], hdr[4], hdr[5], hdr[6], hdr[7],
        ]);
        let data_len = hdr[8] as usize;
        let idx = hdr[9];

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

        Some(LogChunk { id, idx, data })
    }
}

impl LogRing {
    pub const fn new() -> Self {
        Self {
            buf: [0u8; BUF_SIZE],
            head: 0,
            used: 0,
            count: 0,
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

    pub fn push(&mut self, id: u64, idx: u8, data: &[u8]) {
        let data_len = data.len().min(255);
        let frame_size = HEADER_SIZE + data_len;
        if frame_size > BUF_SIZE {
            return;
        }

        // Evict oldest frames until we have space
        while self.used + frame_size > BUF_SIZE {
            self.evict_oldest();
        }

        // Write header
        let write_pos = (self.head + self.used) % BUF_SIZE;
        let id_bytes = id.to_le_bytes();
        let hdr = [
            id_bytes[0],
            id_bytes[1],
            id_bytes[2],
            id_bytes[3],
            id_bytes[4],
            id_bytes[5],
            id_bytes[6],
            id_bytes[7],
            data_len as u8,
            idx,
        ];
        self.write_at(write_pos, &hdr);
        self.write_at((write_pos + HEADER_SIZE) % BUF_SIZE, &data[..data_len]);
        self.used += frame_size;
        self.count += 1;
    }

    fn evict_oldest(&mut self) {
        if self.count == 0 {
            return;
        }
        let mut hdr = [0u8; HEADER_SIZE];
        self.read_at(self.head, &mut hdr);
        let data_len = hdr[8] as usize;
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
    pub fn iter_since(&self, id: u64) -> Iter<'_> {
        if id == 0 {
            return self.iter();
        }

        let mut offset = self.head;
        let mut found_offset = None;
        let mut found_remaining = 0;

        for i in 0..self.count {
            let mut hdr = [0u8; HEADER_SIZE];
            self.read_at(offset, &mut hdr);
            let entry_id = u64::from_le_bytes([
                hdr[0], hdr[1], hdr[2], hdr[3], hdr[4], hdr[5], hdr[6], hdr[7],
            ]);
            let data_len = hdr[8] as usize;
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
