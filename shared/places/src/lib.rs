//! Binary format describing how to reconstitute a firmware image.
//!
//! A `places.bin` file is laid out so that its first byte is the first
//! byte of segment data — typically the bootloader's vector table.
//! When the file is written to flash at a known base address, every
//! segment whose `dest` equals that base + its `file_offset` is XIP-
//! ready: the bytes are already at their linker addresses and need no
//! copy. Segments destined for RAM are copied by the device bootloader
//! the same way as before.
//!
//! Metadata (partition table, segment table, footer) lives at the
//! **end** of the file. The footer's last 4 bytes are the magic
//! `'PLCB'`, so a parser can sniff the format from EOF without knowing
//! the footer size up front.
//!
//! ```text
//! ┌───────────────────────────┐  byte 0
//! │  segment data region      │  XIP segments at fixed offsets;
//! │  (with 0xFF padding)      │  RAM-init segments packed elsewhere
//! ├───────────────────────────┤  byte tables_offset
//! │  Partition[0..P] (16 B)   │
//! ├───────────────────────────┤  byte tables_offset + 16*P
//! │  Segment[0..S] (16 B)     │
//! ├───────────────────────────┤  byte tables_offset + 16*(P+S)
//! │  Footer (24 B)            │
//! └───────────────────────────┘  EOF
//!
//! Footer (read backwards from EOF):
//!   EOF-4  .. EOF    : magic   = 'PLCB'
//!   EOF-8  .. EOF-4  : version = 1
//!   EOF-12 .. EOF-8  : entry_point
//!   EOF-16 .. EOF-12 : partition_count
//!   EOF-20 .. EOF-16 : segment_count
//!   EOF-24 .. EOF-20 : tables_offset
//! ```
//!
//! # XIP convention
//!
//! When the device bootloader processes segments, it should detect
//! self-resident bytes by checking `dest == places_flash_base +
//! segment.file_offset`. For those, the copy is a no-op; XIP just
//! works.

#![no_std]

#[cfg(feature = "std")]
extern crate std;

// ── Format constants ────────────────────────────────────────────────────────

const MAGIC: u32 = 0x504C4342; // 'PLCB'
const VERSION: u32 = 1;
const FOOTER_SIZE: usize = 24;
const PARTITION_SIZE: usize = 16;
const SEGMENT_SIZE: usize = 16;

// ── Partition flags ─────────────────────────────────────────────────────────

/// Partition is not CPU-mapped; accessed via driver (e.g. MPI).
pub const PART_UNMAPPED: u32 = 1 << 0;

/// Partition is owned by a filesystem task — the storage sysmodule
/// rejects direct acquires from anyone else.
pub const PART_MANAGED: u32 = 1 << 1;

// ── Name hashing ────────────────────────────────────────────────────────────

/// FNV-1a hash of a partition name. Deterministic, no_std, and trivial
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

/// A parsed places binary image. Zero-copy — borrows the underlying data.
pub struct PlacesImage<'a> {
    data: &'a [u8],
    entry_point: u32,
    partition_count: u32,
    segment_count: u32,
    tables_offset: u32,
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
    file_offset: u32,
    file_size: u32,
    mem_size: u32,
}

#[derive(Debug)]
pub enum ParseError {
    TooSmall,
    BadMagic,
    BadVersion,
    SegmentOutOfBounds,
    TablesOutOfBounds,
}

impl<'a> PlacesImage<'a> {
    /// Parse a places binary from a byte slice.
    pub fn parse(data: &'a [u8]) -> Result<Self, ParseError> {
        if data.len() < FOOTER_SIZE {
            return Err(ParseError::TooSmall);
        }

        let footer_base = data.len() - FOOTER_SIZE;
        let magic = read_u32(data, footer_base + 20);
        if magic != MAGIC {
            return Err(ParseError::BadMagic);
        }

        let version = read_u32(data, footer_base + 16);
        if version != VERSION {
            return Err(ParseError::BadVersion);
        }

        let entry_point = read_u32(data, footer_base + 12);
        let partition_count = read_u32(data, footer_base + 8);
        let segment_count = read_u32(data, footer_base + 4);
        let tables_offset = read_u32(data, footer_base);

        let part_table_end =
            tables_offset as usize + partition_count as usize * PARTITION_SIZE;
        let seg_table_end =
            part_table_end + segment_count as usize * SEGMENT_SIZE;
        if seg_table_end + FOOTER_SIZE > data.len() {
            return Err(ParseError::TablesOutOfBounds);
        }

        // Validate all segment data ranges are in-bounds.
        for i in 0..segment_count as usize {
            let base = part_table_end + i * SEGMENT_SIZE;
            let file_offset = read_u32(data, base + 4) as usize;
            let file_size = read_u32(data, base + 8) as usize;
            if file_offset + file_size > tables_offset as usize {
                return Err(ParseError::SegmentOutOfBounds);
            }
        }

        Ok(PlacesImage {
            data,
            entry_point,
            partition_count,
            segment_count,
            tables_offset,
        })
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
        let base = self.tables_offset as usize + i as usize * PARTITION_SIZE;
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
        let seg_table_base = self.tables_offset as usize
            + self.partition_count as usize * PARTITION_SIZE;
        let base = seg_table_base + i as usize * SEGMENT_SIZE;
        let dest = read_u32(self.data, base);
        let file_offset = read_u32(self.data, base + 4);
        let file_size = read_u32(self.data, base + 8);
        let mem_size = read_u32(self.data, base + 12);
        let data = &self.data[file_offset as usize..file_offset as usize + file_size as usize];
        Segment { data, dest, file_offset, file_size, mem_size }
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

    /// Offset of this segment's bytes within the places.bin file.
    /// For an XIP segment, `places_flash_base + file_offset == dest`.
    pub fn file_offset(&self) -> u32 {
        self.file_offset
    }

    /// Bytes to copy from the image.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Number of bytes to copy (same as `data().len()`).
    pub fn file_size(&self) -> u32 {
        self.file_size
    }

    /// Total size in memory. If greater than `file_size`, the remaining
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
    file_offset: u32,
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

    /// Add a segment with an explicit file offset.
    ///
    /// The caller is responsible for choosing non-overlapping
    /// `file_offset` values across segments. For XIP segments backed
    /// by the place that hosts the places.bin file, set
    /// `file_offset = dest - place_flash_base`. For RAM-destined
    /// segments, pick any unclaimed offset.
    ///
    /// - `dest`: target address in memory.
    /// - `file_offset`: byte offset within places.bin where `data` lives.
    /// - `data`: bytes to write into the file at `file_offset`.
    /// - `mem_size`: total in-memory size (>= `data.len()`).
    pub fn add_segment(&mut self, dest: u32, file_offset: u32, data: &[u8], mem_size: u32) {
        assert!(mem_size as usize >= data.len());
        self.segments.push(BuilderSegment {
            dest,
            file_offset,
            data: data.to_vec(),
            mem_size,
        });
    }

    /// Build the places binary.
    pub fn build(self) -> std::vec::Vec<u8> {
        let partition_count = self.partitions.len();
        let segment_count = self.segments.len();

        // Data region ends at the highest (file_offset + data.len()) across
        // all segments. Aligned to 4 bytes for clean table placement.
        let data_end: usize = self
            .segments
            .iter()
            .map(|s| s.file_offset as usize + s.data.len())
            .max()
            .unwrap_or(0);
        let tables_offset = (data_end + 3) & !3;

        let part_table_size = partition_count * PARTITION_SIZE;
        let seg_table_size = segment_count * SEGMENT_SIZE;
        let total = tables_offset + part_table_size + seg_table_size + FOOTER_SIZE;

        let mut out = std::vec![0xFFu8; total];

        // Data region: write each segment at its file_offset.
        for seg in &self.segments {
            let off = seg.file_offset as usize;
            let end = off + seg.data.len();
            assert!(
                end <= tables_offset,
                "segment at offset {off:#x} (len {}) overlaps tables at {tables_offset:#x}",
                seg.data.len()
            );
            out[off..end].copy_from_slice(&seg.data);
        }

        // Partition table.
        for (i, part) in self.partitions.iter().enumerate() {
            let base = tables_offset + i * PARTITION_SIZE;
            write_u32(&mut out, base, part.name_hash);
            write_u32(&mut out, base + 4, part.offset);
            write_u32(&mut out, base + 8, part.size);
            write_u32(&mut out, base + 12, part.flags);
        }

        // Segment table.
        let seg_table_base = tables_offset + part_table_size;
        for (i, seg) in self.segments.iter().enumerate() {
            let base = seg_table_base + i * SEGMENT_SIZE;
            write_u32(&mut out, base, seg.dest);
            write_u32(&mut out, base + 4, seg.file_offset);
            write_u32(&mut out, base + 8, seg.data.len() as u32);
            write_u32(&mut out, base + 12, seg.mem_size);
        }

        // Footer (last 24 bytes). Magic at the very end.
        let footer_base = total - FOOTER_SIZE;
        write_u32(&mut out, footer_base, tables_offset as u32);
        write_u32(&mut out, footer_base + 4, segment_count as u32);
        write_u32(&mut out, footer_base + 8, partition_count as u32);
        write_u32(&mut out, footer_base + 12, self.entry_point);
        write_u32(&mut out, footer_base + 16, VERSION);
        write_u32(&mut out, footer_base + 20, MAGIC);

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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_xip_and_ram_segments() {
        let mut b = PlacesBuilder::new(0x1000_0100);
        b.add_partition(name_hash(b"image"), 0x5000, 0x10_0000, 0);
        b.add_partition(name_hash(b"logs"), 0x110_5000, 0x10_0000, PART_UNMAPPED);

        // XIP: flash-resident bytes at file_offset 0 (matches dest=image_base).
        let xip_bytes: std::vec::Vec<u8> = (0..64u8).collect();
        b.add_segment(0x1200_0000, 0, &xip_bytes, 64);

        // RAM-init: bytes packed after XIP region; dest is in RAM.
        let ram_init: std::vec::Vec<u8> = (0..32u8).map(|x| x ^ 0xAA).collect();
        b.add_segment(0x2000_0000, 64, &ram_init, 64);

        let bytes = b.build();
        let img = PlacesImage::parse(&bytes).expect("parse");

        assert_eq!(img.entry_point(), 0x1000_0100);
        assert_eq!(img.partition_count(), 2);
        assert_eq!(img.segment_count(), 2);

        let s0 = img.segment(0);
        assert_eq!(s0.dest(), 0x1200_0000);
        assert_eq!(s0.file_offset(), 0);
        assert_eq!(s0.data(), &xip_bytes[..]);

        let s1 = img.segment(1);
        assert_eq!(s1.dest(), 0x2000_0000);
        assert_eq!(s1.file_offset(), 64);
        assert_eq!(s1.data(), &ram_init[..]);
        assert_eq!(s1.zero_fill(), 32);

        // Magic at the very end.
        assert_eq!(&bytes[bytes.len() - 4..], &MAGIC.to_le_bytes());
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut bytes = std::vec![0u8; 24];
        bytes[20..24].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        assert!(matches!(PlacesImage::parse(&bytes), Err(ParseError::BadMagic)));
    }
}
