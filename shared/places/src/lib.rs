//! Binary format for loading firmware segments into memory.
//!
//! A `places.bin` file contains a header, a partition table, a segment
//! table, and packed segment data.  The bootloader reads segments from
//! flash and copies each one to its destination address in memory.
//! The partition table describes the full flash layout so the running
//! firmware can discover persistent partitions (logs, filesystems, etc.)
//! at runtime.
//!
//! ```text
//! ┌─────────────────────────────┐  byte 0
//! │  Header (20 bytes)          │
//! ├─────────────────────────────┤  byte 20
//! │  Partition[0] (16 bytes)    │
//! │  Partition[1]               │
//! │  ...                        │
//! ├─────────────────────────────┤  byte 20 + 16*P
//! │  Segment[0] (16 bytes)      │
//! │  Segment[1]                 │
//! │  ...                        │
//! ├─────────────────────────────┤  byte 20 + 16*P + 16*S
//! │  packed segment data        │
//! └─────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! Reading (works on `no_std`):
//!
//! ```ignore
//! let image = PlacesImage::parse(data)?;
//! for seg in image.segments() {
//!     copy(seg.dest(), seg.data(), seg.zero_fill());
//! }
//! set_vtor(image.entry_point());
//! ```
//!
//! Building (requires `std` feature):
//!
//! ```ignore
//! let mut b = PlacesBuilder::new(entry_point);
//! b.add_partition(name_hash, offset, size, flags);
//! b.add_segment(dest, &data, mem_size);
//! let bytes = b.build();
//! ```

#![no_std]

#[cfg(feature = "std")]
extern crate std;

// ── Format constants ────────────────────────────────────────────────────────

const MAGIC: u32 = 0x504C4342; // "PLCB" (places binary)
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 20;
const PARTITION_SIZE: usize = 16;
const SEGMENT_SIZE: usize = 16;

// ── Partition flags ─────────────────────────────────────────────────────────

/// Partition is not CPU-mapped; accessed via driver (e.g. MPI).
pub const PART_UNMAPPED: u32 = 1 << 0;

// ── Name hashing ────────────────────────────────────────────────────────────

/// FNV-1a hash of a partition name.  Deterministic, no_std, and trivial
/// to compute on both host and device.
pub const fn name_hash(name: &[u8]) -> u32 {
    const FNV_OFFSET: u32 = 0x811c_9dc5;
    const FNV_PRIME: u32 = 0x0100_0193;
    let mut h = FNV_OFFSET;
    let mut i = 0;
    while i < name.len() {
        h ^= name[i] as u32;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    h
}

// ── Reader (no_std) ─────────────────────────────────────────────────────────

/// A parsed places binary image.  Zero-copy — borrows the underlying data.
pub struct PlacesImage<'a> {
    data: &'a [u8],
    entry_point: u32,
    partition_count: u32,
    segment_count: u32,
}

/// A flash partition entry within a [`PlacesImage`].
#[derive(Clone, Copy)]
pub struct Partition {
    pub name_hash: u32,
    pub offset: u32,
    pub size: u32,
    pub flags: u32,
}

/// A single load segment within a [`PlacesImage`].
pub struct Segment<'a> {
    data: &'a [u8],
    dest: u32,
    file_size: u32,
    mem_size: u32,
}

#[derive(Debug)]
pub enum ParseError {
    TooSmall,
    BadMagic,
    BadVersion,
    SegmentOutOfBounds,
}

impl<'a> PlacesImage<'a> {
    /// Parse a places binary from a byte slice.
    pub fn parse(data: &'a [u8]) -> Result<Self, ParseError> {
        if data.len() < HEADER_SIZE {
            return Err(ParseError::TooSmall);
        }

        let magic = read_u32(data, 0);
        if magic != MAGIC {
            return Err(ParseError::BadMagic);
        }

        let version = read_u32(data, 4);
        if version != VERSION {
            return Err(ParseError::BadVersion);
        }

        let entry_point = read_u32(data, 8);
        let partition_count = read_u32(data, 12);
        let segment_count = read_u32(data, 16);

        let seg_table_start = HEADER_SIZE + partition_count as usize * PARTITION_SIZE;
        let table_end = seg_table_start + segment_count as usize * SEGMENT_SIZE;
        if data.len() < table_end {
            return Err(ParseError::TooSmall);
        }

        // Validate all segment data ranges are in-bounds.
        for i in 0..segment_count as usize {
            let base = seg_table_start + i * SEGMENT_SIZE;
            let offset = read_u32(data, base + 4) as usize;
            let file_size = read_u32(data, base + 8) as usize;
            if offset + file_size > data.len() {
                return Err(ParseError::SegmentOutOfBounds);
            }
        }

        Ok(PlacesImage { data, entry_point, partition_count, segment_count })
    }

    /// Address of the kernel vector table.
    pub fn entry_point(&self) -> u32 {
        self.entry_point
    }

    /// Number of partitions.
    pub fn partition_count(&self) -> u32 {
        self.partition_count
    }

    /// Number of segments.
    pub fn segment_count(&self) -> u32 {
        self.segment_count
    }

    /// Get partition `i`.
    ///
    /// # Panics
    ///
    /// Panics if `i >= partition_count()`.
    pub fn partition(&self, i: u32) -> Partition {
        assert!(i < self.partition_count);
        let base = HEADER_SIZE + i as usize * PARTITION_SIZE;
        Partition {
            name_hash: read_u32(self.data, base),
            offset: read_u32(self.data, base + 4),
            size: read_u32(self.data, base + 8),
            flags: read_u32(self.data, base + 12),
        }
    }

    /// Find a partition by name hash.
    pub fn find_partition(&self, hash: u32) -> Option<Partition> {
        for i in 0..self.partition_count {
            let p = self.partition(i);
            if p.name_hash == hash {
                return Some(p);
            }
        }
        None
    }

    /// Get segment `i`.
    ///
    /// # Panics
    ///
    /// Panics if `i >= segment_count()`.
    pub fn segment(&self, i: u32) -> Segment<'a> {
        assert!(i < self.segment_count);
        let seg_table_start = HEADER_SIZE + self.partition_count as usize * PARTITION_SIZE;
        let base = seg_table_start + i as usize * SEGMENT_SIZE;
        let dest = read_u32(self.data, base);
        let offset = read_u32(self.data, base + 4) as usize;
        let file_size = read_u32(self.data, base + 8);
        let mem_size = read_u32(self.data, base + 12);
        let data = &self.data[offset..offset + file_size as usize];
        Segment { data, dest, file_size, mem_size }
    }

    /// Iterate all segments.
    pub fn segments(&'a self) -> SegmentIter<'a> {
        SegmentIter { image: self, index: 0 }
    }

    /// Iterate all partitions.
    pub fn partitions(&self) -> PartitionIter<'_> {
        PartitionIter { image: self, index: 0 }
    }
}

impl<'a> Segment<'a> {
    /// Destination address in memory.
    pub fn dest(&self) -> u32 {
        self.dest
    }

    /// Bytes to copy from the image.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Number of bytes to copy (same as `data().len()`).
    pub fn file_size(&self) -> u32 {
        self.file_size
    }

    /// Total size in memory.  If greater than `file_size`, the remaining
    /// bytes should be zero-filled (this covers `.bss`).
    pub fn mem_size(&self) -> u32 {
        self.mem_size
    }

    /// Number of bytes to zero-fill after copying `data()`.
    pub fn zero_fill(&self) -> u32 {
        self.mem_size.saturating_sub(self.file_size)
    }
}

pub struct SegmentIter<'a> {
    image: &'a PlacesImage<'a>,
    index: u32,
}

impl<'a> Iterator for SegmentIter<'a> {
    type Item = Segment<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.image.segment_count {
            return None;
        }
        let seg = self.image.segment(self.index);
        self.index += 1;
        Some(seg)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.image.segment_count - self.index) as usize;
        (remaining, Some(remaining))
    }
}

impl<'a> ExactSizeIterator for SegmentIter<'a> {}

pub struct PartitionIter<'a> {
    image: &'a PlacesImage<'a>,
    index: u32,
}

impl<'a> Iterator for PartitionIter<'a> {
    type Item = Partition;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.image.partition_count {
            return None;
        }
        let part = self.image.partition(self.index);
        self.index += 1;
        Some(part)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.image.partition_count - self.index) as usize;
        (remaining, Some(remaining))
    }
}

impl<'a> ExactSizeIterator for PartitionIter<'a> {}

// ── Builder (std only) ───────────────────────────────────────────────────────

#[cfg(feature = "std")]
pub struct PlacesBuilder {
    entry_point: u32,
    partitions: std::vec::Vec<Partition>,
    segments: std::vec::Vec<BuilderSegment>,
}

#[cfg(feature = "std")]
struct BuilderSegment {
    dest: u32,
    data: std::vec::Vec<u8>,
    mem_size: u32,
}

#[cfg(feature = "std")]
impl PlacesBuilder {
    /// Create a new builder with the given kernel entry point.
    pub fn new(entry_point: u32) -> Self {
        PlacesBuilder {
            entry_point,
            partitions: std::vec::Vec::new(),
            segments: std::vec::Vec::new(),
        }
    }

    /// Add a flash partition entry.
    pub fn add_partition(&mut self, name_hash: u32, offset: u32, size: u32, flags: u32) {
        self.partitions.push(Partition { name_hash, offset, size, flags });
    }

    /// Add a segment.
    ///
    /// - `dest`: target address in memory
    /// - `data`: bytes to copy
    /// - `mem_size`: total size in memory (must be >= `data.len()`)
    pub fn add_segment(&mut self, dest: u32, data: &[u8], mem_size: u32) {
        assert!(mem_size as usize >= data.len());
        self.segments.push(BuilderSegment {
            dest,
            data: data.to_vec(),
            mem_size,
        });
    }

    /// Build the places binary.
    pub fn build(self) -> std::vec::Vec<u8> {
        let partition_count = self.partitions.len();
        let segment_count = self.segments.len();
        let seg_table_start = HEADER_SIZE + partition_count * PARTITION_SIZE;
        let table_end = seg_table_start + segment_count * SEGMENT_SIZE;

        // Compute offsets for each segment's data.
        let mut data_offset = table_end;
        let mut offsets = std::vec::Vec::with_capacity(segment_count);
        for seg in &self.segments {
            offsets.push(data_offset);
            data_offset += seg.data.len();
        }

        let total = data_offset;
        let mut out = std::vec![0u8; total];

        // Header
        write_u32(&mut out, 0, MAGIC);
        write_u32(&mut out, 4, VERSION);
        write_u32(&mut out, 8, self.entry_point);
        write_u32(&mut out, 12, partition_count as u32);
        write_u32(&mut out, 16, segment_count as u32);

        // Partition table
        for (i, part) in self.partitions.iter().enumerate() {
            let base = HEADER_SIZE + i * PARTITION_SIZE;
            write_u32(&mut out, base, part.name_hash);
            write_u32(&mut out, base + 4, part.offset);
            write_u32(&mut out, base + 8, part.size);
            write_u32(&mut out, base + 12, part.flags);
        }

        // Segment table + data
        for (i, seg) in self.segments.iter().enumerate() {
            let base = seg_table_start + i * SEGMENT_SIZE;
            write_u32(&mut out, base, seg.dest);
            write_u32(&mut out, base + 4, offsets[i] as u32);
            write_u32(&mut out, base + 8, seg.data.len() as u32);
            write_u32(&mut out, base + 12, seg.mem_size);

            out[offsets[i]..offsets[i] + seg.data.len()]
                .copy_from_slice(&seg.data);
        }

        out
    }
}

// ── Byte helpers (little-endian) ─────────────────────────────────────────────

fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

#[cfg(feature = "std")]
fn write_u32(data: &mut [u8], offset: usize, val: u32) {
    data[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}
