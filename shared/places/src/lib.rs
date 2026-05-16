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
//! ├───────────────────────────┤
//! │  Footer (v1: 24 B,        │
//! │          v2: 48 B)        │
//! └───────────────────────────┘  EOF
//! ```
//!
//! # v2 footer layout (read backwards from EOF)
//!
//! ```text
//!   EOF-48 .. EOF-44 : checksum         (CRC-32, excludes self and state)
//!   EOF-44 .. EOF-42 : ver_major        (u16)
//!   EOF-42 .. EOF-40 : ver_minor        (u16)
//!   EOF-40 .. EOF-38 : ver_patch        (u16)
//!   EOF-38 .. EOF-36 : _reserved        (u16, 0xFFFF)
//!   EOF-36 .. EOF-28 : flash_timestamp  (u64, unix seconds)
//!   EOF-28 .. EOF-24 : state            (u32, writable by runtime)
//!   EOF-24 .. EOF-20 : tables_offset    (u32)
//!   EOF-20 .. EOF-16 : segment_count    (u32)
//!   EOF-16 .. EOF-12 : partition_count  (u32)
//!   EOF-12 .. EOF-8  : entry_point      (u32)
//!   EOF-8  .. EOF-4  : format_version   (u32, = 2)
//!   EOF-4  .. EOF    : magic            (u32, = 'PLCB')
//! ```
//!
//! # XIP convention
//!
//! When the device bootloader processes segments, it should detect
//! self-resident bytes by checking `dest == places_flash_base +
//! segment.file_offset`. For those, the copy is a no-op; XIP just
//! works.
//!
//! # A/B firmware selection
//!
//! The bootloader parses both firmware slots (A and B), examines each
//! image's [`FirmwareState`], and calls [`select_firmware`] to pick
//! the winner. An image is bootable only if its state is `Default`
//! (checksum passes and no runtime concern has been written).

#![no_std]

#[cfg(feature = "std")]
extern crate std;

// ── Format constants ────────────────────────────────────────────────────────

const MAGIC: u32 = 0x504C4342; // 'PLCB'
const VERSION_1: u32 = 1;
const VERSION_2: u32 = 2;
const FOOTER_V1_SIZE: usize = 24;
const FOOTER_V2_SIZE: usize = 48;
const PARTITION_SIZE: usize = 16;
const SEGMENT_SIZE: usize = 16;

// ── Partition flags ─────────────────────────────────────────────────────────

/// Partition is not CPU-mapped; accessed via driver (e.g. MPI).
pub const PART_UNMAPPED: u32 = 1 << 0;

/// Partition is owned by a filesystem task — the storage sysmodule
/// rejects direct acquires from anyone else.
pub const PART_MANAGED: u32 = 1 << 1;

// ── CRC-32 (IEEE 802.3) ────────────────────────────────────────────────────

const CRC32_POLY: u32 = 0xEDB8_8320;
const CRC32_INIT: u32 = 0xFFFF_FFFF;

const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i: usize = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ CRC32_POLY;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    let mut i = 0;
    while i < data.len() {
        // SAFETY: i < data.len() by loop guard; index <= 0xFF by mask.
        let index = ((crc ^ unsafe { *data.get_unchecked(i) } as u32) & 0xFF) as usize;
        crc = (crc >> 8) ^ unsafe { *CRC32_TABLE.get_unchecked(index) };
        i += 1;
    }
    crc
}

/// Compute CRC-32 (IEEE 802.3) over a byte slice.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(CRC32_INIT, data) ^ CRC32_INIT
}

/// Compute the v2 image checksum. Covers everything except the
/// checksum field (4 B at EOF-48) and the state field (4 B at EOF-28).
fn compute_checksum_v2(data: &[u8]) -> u32 {
    if data.len() < FOOTER_V2_SIZE {
        return 0;
    }
    let fb = data.len() - FOOTER_V2_SIZE;
    let mut c = CRC32_INIT;
    // SAFETY: fb + 24 < fb + FOOTER_V2_SIZE == data.len(), all in bounds.
    unsafe {
        c = crc32_update(c, data.get_unchecked(..fb));
        c = crc32_update(c, data.get_unchecked(fb + 4..fb + 20));
        c = crc32_update(c, data.get_unchecked(fb + 24..));
    }
    c ^ CRC32_INIT
}

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

// ── Firmware state ──────────────────────────────────────────────────────────

const STATE_DEFAULT_RAW: u32 = 0xFFFF_FFFF;

/// Source of a firmware concern, stored in bits 31..24 of the state
/// field. The runtime writes a concern as a single flash program
/// operation (no erase needed — all bits go from 1 to 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConcernSource {
    Watchdog = 1,
    Kernel = 2,
    Supervisor = 3,
    Runtime = 4,
}

impl ConcernSource {
    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Watchdog),
            2 => Some(Self::Kernel),
            3 => Some(Self::Supervisor),
            4 => Some(Self::Runtime),
            _ => None,
        }
    }
}

/// Computed firmware state for A/B slot selection.
///
/// The bootloader derives this for each slot by checking the CRC-32
/// checksum (→ `IntegrityCompromised` if it fails) and then decoding
/// the on-flash state field (→ a concern variant if a runtime
/// component wrote one, or `Default` if the field is still erased).
///
/// Only `Default` is bootable. [`select_firmware`] uses this to pick
/// the winning slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareState {
    /// CRC-32 checksum does not match — image is corrupt or incomplete.
    IntegrityCompromised,
    /// The bootloader's watchdog flagged a concern.
    WatchdogConcern,
    /// The kernel flagged a concern.
    KernelConcern,
    /// The supervisor flagged a concern.
    SupervisorConcern,
    /// The runtime flagged a concern.
    RuntimeConcern,
    /// No issues detected — image is bootable.
    Default,
}

impl FirmwareState {
    /// Returns `true` only for `Default` — the only bootable state.
    pub fn is_bootable(&self) -> bool {
        matches!(self, Self::Default)
    }

    /// Encode a concern as a raw u32 for writing to the on-flash state
    /// field. The source tag is stored in bits 31..24; bits 23..0 are
    /// reserved for future per-variant payloads.
    pub const fn encode_concern(source: ConcernSource) -> u32 {
        (source as u32) << 24
    }

    /// Which [`ConcernSource`] this state represents, if any.
    pub fn concern_source(&self) -> Option<ConcernSource> {
        match self {
            Self::WatchdogConcern => Some(ConcernSource::Watchdog),
            Self::KernelConcern => Some(ConcernSource::Kernel),
            Self::SupervisorConcern => Some(ConcernSource::Supervisor),
            Self::RuntimeConcern => Some(ConcernSource::Runtime),
            _ => None,
        }
    }

    fn decode_stored(raw: u32) -> Self {
        if raw == STATE_DEFAULT_RAW {
            return Self::Default;
        }
        let tag = (raw >> 24) as u8;
        match ConcernSource::from_tag(tag) {
            Some(ConcernSource::Watchdog) => Self::WatchdogConcern,
            Some(ConcernSource::Kernel) => Self::KernelConcern,
            Some(ConcernSource::Supervisor) => Self::SupervisorConcern,
            Some(ConcernSource::Runtime) => Self::RuntimeConcern,
            None => Self::RuntimeConcern,
        }
    }
}

// ── Reader (no_std) ─────────────────────────────────────────────────────────

/// A parsed places binary image. Zero-copy — borrows the underlying data.
pub struct PlacesImage<'a> {
    data: &'a [u8],
    format_version: u32,
    entry_point: u32,
    partition_count: u32,
    segment_count: u32,
    tables_offset: u32,
    // v2 metadata (zeroed/defaulted for v1 images).
    checksum: u32,
    version: (u16, u16, u16),
    flash_timestamp: u64,
    state_raw: u32,
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
        if data.len() < FOOTER_V1_SIZE {
            return Err(ParseError::TooSmall);
        }

        let magic = read_u32(data, data.len() - 4);
        if magic != MAGIC {
            return Err(ParseError::BadMagic);
        }

        let format_version = read_u32(data, data.len() - 8);

        let (footer_size, checksum, version, flash_timestamp, state_raw) = match format_version {
            VERSION_1 => (
                FOOTER_V1_SIZE,
                0u32,
                (0u16, 0u16, 0u16),
                0u64,
                STATE_DEFAULT_RAW,
            ),
            VERSION_2 => {
                if data.len() < FOOTER_V2_SIZE {
                    return Err(ParseError::TooSmall);
                }
                let fb = data.len() - FOOTER_V2_SIZE;
                (
                    FOOTER_V2_SIZE,
                    read_u32(data, fb),
                    (
                        read_u16(data, fb + 4),
                        read_u16(data, fb + 6),
                        read_u16(data, fb + 8),
                    ),
                    read_u64(data, fb + 12),
                    read_u32(data, fb + 20),
                )
            }
            _ => return Err(ParseError::BadVersion),
        };

        // Core footer fields (same position relative to EOF in both versions).
        let core_base = data.len() - FOOTER_V1_SIZE;
        let tables_offset = read_u32(data, core_base);
        let segment_count = read_u32(data, core_base + 4);
        let partition_count = read_u32(data, core_base + 8);
        let entry_point = read_u32(data, core_base + 12);

        let part_table_end = tables_offset as usize + partition_count as usize * PARTITION_SIZE;
        let seg_table_end = part_table_end + segment_count as usize * SEGMENT_SIZE;
        if seg_table_end + footer_size > data.len() {
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
            format_version,
            entry_point,
            partition_count,
            segment_count,
            tables_offset,
            checksum,
            version,
            flash_timestamp,
            state_raw,
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

    /// Format version (1 or 2).
    pub fn format_version(&self) -> u32 {
        self.format_version
    }

    /// Firmware version as (major, minor, patch). Returns (0, 0, 0)
    /// for v1 images.
    pub fn version(&self) -> (u16, u16, u16) {
        self.version
    }

    /// Flash timestamp (unix seconds, set by host tool). Returns 0
    /// for v1 images.
    pub fn flash_timestamp(&self) -> u64 {
        self.flash_timestamp
    }

    /// Verify the CRC-32 checksum. Always returns `true` for v1
    /// images (which have no checksum).
    pub fn checksum_valid(&self) -> bool {
        match self.format_version {
            VERSION_1 => true,
            VERSION_2 => compute_checksum_v2(self.data) == self.checksum,
            _ => false,
        }
    }

    /// Compute the firmware state for A/B selection. Checks integrity
    /// first (CRC-32), then decodes the stored state field.
    pub fn firmware_state(&self) -> FirmwareState {
        if self.format_version >= VERSION_2 && !self.checksum_valid() {
            return FirmwareState::IntegrityCompromised;
        }
        FirmwareState::decode_stored(self.state_raw)
    }

    /// Get partition `i`.
    ///
    /// # Safety
    ///
    /// `i` must be less than `partition_count()`.
    pub unsafe fn partition(&self, i: u32) -> Partition {
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
            // SAFETY: i < partition_count.
            let p = unsafe { self.partition(i) };
            if p.name_hash == hash {
                return Some(p);
            }
        }
        None
    }

    /// Get segment `i`.
    ///
    /// # Safety
    ///
    /// `i` must be less than `segment_count()`.
    pub unsafe fn segment(&self, i: u32) -> Segment<'a> {
        let seg_table_base =
            self.tables_offset as usize + self.partition_count as usize * PARTITION_SIZE;
        let base = seg_table_base + i as usize * SEGMENT_SIZE;
        let dest = read_u32(self.data, base);
        let file_offset = read_u32(self.data, base + 4);
        let file_size = read_u32(self.data, base + 8);
        let mem_size = read_u32(self.data, base + 12);
        let start = file_offset as usize;
        let end = start + file_size as usize;
        let data = unsafe { self.data.get_unchecked(start..end) };
        Segment {
            data,
            dest,
            file_offset,
            file_size,
            mem_size,
        }
    }

    /// Iterate all segments.
    pub fn segments(&'a self) -> SegmentIter<'a> {
        SegmentIter {
            image: self,
            index: 0,
        }
    }

    /// Iterate all partitions.
    pub fn partitions(&self) -> PartitionIter<'_> {
        PartitionIter {
            image: self,
            index: 0,
        }
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
        // SAFETY: index < segment_count is checked above.
        let seg = unsafe { self.image.segment(self.index) };
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
        // SAFETY: index < partition_count is checked above.
        let part = unsafe { self.image.partition(self.index) };
        self.index += 1;
        Some(part)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.image.partition_count - self.index) as usize;
        (remaining, Some(remaining))
    }
}

impl<'a> ExactSizeIterator for PartitionIter<'a> {}

// ── State field offset ──────────────────────────────────────────────────────

/// Byte offset of the state field from the end of a v2 places image.
///
/// The runtime writes a concern to the on-flash state field at:
///   `places_flash_base + places_size - STATE_OFFSET_FROM_END`
///
/// The write is a single u32 program operation (no erase needed — bits
/// go from the erased-flash 0xFF default to 0 only).
pub const STATE_OFFSET_FROM_END: usize = 28;

// ── Slot selection ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

/// Why [`select_firmware`] picked a particular slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionReason {
    /// The other slot had a concern or failed its checksum.
    OtherConcerned(FirmwareState),
    /// Both bootable; this slot has a higher semver.
    HigherVersion,
    /// Both bootable, same version; this slot was flashed more recently.
    NewerFlash,
    /// Both bootable, identical version and timestamp; slot A wins.
    Tiebreak,
}

#[derive(Debug)]
pub enum SelectionError {
    BothConcerned { a: FirmwareState, b: FirmwareState },
}

/// Pick which firmware slot to boot.
///
/// 1. If exactly one slot is bootable (`Default`), pick it.
/// 2. If neither is bootable, return [`SelectionError::BothConcerned`].
/// 3. If both are bootable, prefer higher semver, then newer flash
///    timestamp, then slot A.
pub fn select_firmware(
    a: &PlacesImage,
    b: &PlacesImage,
) -> Result<(Slot, SelectionReason), SelectionError> {
    let a_state = a.firmware_state();
    let b_state = b.firmware_state();

    match (a_state.is_bootable(), b_state.is_bootable()) {
        (true, false) => return Ok((Slot::A, SelectionReason::OtherConcerned(b_state))),
        (false, true) => return Ok((Slot::B, SelectionReason::OtherConcerned(a_state))),
        (false, false) => {
            return Err(SelectionError::BothConcerned {
                a: a_state,
                b: b_state,
            })
        }
        (true, true) => {}
    }

    match cmp_version(a.version(), b.version()) {
        core::cmp::Ordering::Greater => return Ok((Slot::A, SelectionReason::HigherVersion)),
        core::cmp::Ordering::Less => return Ok((Slot::B, SelectionReason::HigherVersion)),
        core::cmp::Ordering::Equal => {}
    }

    match a.flash_timestamp().cmp(&b.flash_timestamp()) {
        core::cmp::Ordering::Greater => return Ok((Slot::A, SelectionReason::NewerFlash)),
        core::cmp::Ordering::Less => return Ok((Slot::B, SelectionReason::NewerFlash)),
        core::cmp::Ordering::Equal => {}
    }

    Ok((Slot::A, SelectionReason::Tiebreak))
}

fn cmp_version(a: (u16, u16, u16), b: (u16, u16, u16)) -> core::cmp::Ordering {
    a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2))
}

// ── Builder (std only) ──────────────────────────────────────────────────────

#[cfg(feature = "std")]
pub struct PlacesBuilder {
    entry_point: u32,
    version: (u16, u16, u16),
    flash_timestamp: u64,
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
            version: (0, 0, 0),
            flash_timestamp: 0,
            partitions: std::vec::Vec::new(),
            segments: std::vec::Vec::new(),
        }
    }

    /// Set the firmware semver version.
    pub fn set_version(&mut self, major: u16, minor: u16, patch: u16) {
        self.version = (major, minor, patch);
    }

    /// Set the flash timestamp (unix seconds, from host clock).
    pub fn set_flash_timestamp(&mut self, secs: u64) {
        self.flash_timestamp = secs;
    }

    /// Add a flash partition entry.
    pub fn add_partition(&mut self, name_hash: u32, offset: u32, size: u32, flags: u32) {
        self.partitions.push(Partition {
            name_hash,
            offset,
            size,
            flags,
        });
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

    /// Build the places binary (v2 format with checksum).
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
        let total = tables_offset + part_table_size + seg_table_size + FOOTER_V2_SIZE;

        let mut out = std::vec![0xFFu8; total];

        // Data region: write each segment at its file_offset.
        for seg in &self.segments {
            let off = seg.file_offset as usize;
            let end = off + seg.data.len();
            assert!(
                end <= tables_offset,
                "segment at offset {off:#x} (len {}) overlaps tables at \
                 {tables_offset:#x}",
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

        // v2 extended footer (checksum placeholder — filled last).
        let fb = total - FOOTER_V2_SIZE;
        write_u32(&mut out, fb, 0); // checksum placeholder
        write_u16(&mut out, fb + 4, self.version.0);
        write_u16(&mut out, fb + 6, self.version.1);
        write_u16(&mut out, fb + 8, self.version.2);
        write_u16(&mut out, fb + 10, 0xFFFF); // reserved
        write_u64(&mut out, fb + 12, self.flash_timestamp);
        write_u32(&mut out, fb + 20, STATE_DEFAULT_RAW);

        // Core footer (last 24 bytes — layout-compatible with v1).
        write_u32(&mut out, fb + 24, tables_offset as u32);
        write_u32(&mut out, fb + 28, segment_count as u32);
        write_u32(&mut out, fb + 32, partition_count as u32);
        write_u32(&mut out, fb + 36, self.entry_point);
        write_u32(&mut out, fb + 40, VERSION_2);
        write_u32(&mut out, fb + 44, MAGIC);

        // Compute and write checksum.
        let checksum = compute_checksum_v2(&out);
        write_u32(&mut out, fb, checksum);

        out
    }
}

// ── Byte helpers (little-endian) ─────────────────────────────────────────────

/// # Safety
/// Caller must ensure `offset + 4 <= data.len()`. All call sites are
/// guarded by the bounds checks in `PlacesImage::parse`.
fn read_u32(data: &[u8], offset: usize) -> u32 {
    unsafe {
        u32::from_le_bytes([
            *data.get_unchecked(offset),
            *data.get_unchecked(offset + 1),
            *data.get_unchecked(offset + 2),
            *data.get_unchecked(offset + 3),
        ])
    }
}

fn read_u16(data: &[u8], offset: usize) -> u16 {
    unsafe { u16::from_le_bytes([*data.get_unchecked(offset), *data.get_unchecked(offset + 1)]) }
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    unsafe {
        u64::from_le_bytes([
            *data.get_unchecked(offset),
            *data.get_unchecked(offset + 1),
            *data.get_unchecked(offset + 2),
            *data.get_unchecked(offset + 3),
            *data.get_unchecked(offset + 4),
            *data.get_unchecked(offset + 5),
            *data.get_unchecked(offset + 6),
            *data.get_unchecked(offset + 7),
        ])
    }
}

#[cfg(feature = "std")]
fn write_u32(data: &mut [u8], offset: usize, val: u32) {
    data[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
}

#[cfg(feature = "std")]
fn write_u16(data: &mut [u8], offset: usize, val: u16) {
    data[offset..offset + 2].copy_from_slice(&val.to_le_bytes());
}

#[cfg(feature = "std")]
fn write_u64(data: &mut [u8], offset: usize, val: u64) {
    data[offset..offset + 8].copy_from_slice(&val.to_le_bytes());
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_v2_xip_and_ram_segments() {
        let mut b = PlacesBuilder::new(0x1000_0100);
        b.set_version(1, 4, 2);
        b.set_flash_timestamp(1_700_000_000);
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

        assert_eq!(img.format_version(), 2);
        assert_eq!(img.entry_point(), 0x1000_0100);
        assert_eq!(img.version(), (1, 4, 2));
        assert_eq!(img.flash_timestamp(), 1_700_000_000);
        assert_eq!(img.partition_count(), 2);
        assert_eq!(img.segment_count(), 2);
        assert!(img.checksum_valid());
        assert_eq!(img.firmware_state(), FirmwareState::Default);

        let s0 = unsafe { img.segment(0) };
        assert_eq!(s0.dest(), 0x1200_0000);
        assert_eq!(s0.file_offset(), 0);
        assert_eq!(s0.data(), &xip_bytes[..]);

        let s1 = unsafe { img.segment(1) };
        assert_eq!(s1.dest(), 0x2000_0000);
        assert_eq!(s1.file_offset(), 64);
        assert_eq!(s1.data(), &ram_init[..]);
        assert_eq!(s1.zero_fill(), 32);

        // Magic at the very end.
        assert_eq!(&bytes[bytes.len() - 4..], &MAGIC.to_le_bytes());
    }

    #[test]
    fn corrupted_data_fails_checksum() {
        let mut b = PlacesBuilder::new(0x1000_0100);
        b.set_version(1, 0, 0);
        let data: std::vec::Vec<u8> = (0..32u8).collect();
        b.add_segment(0x1200_0000, 0, &data, 32);

        let mut bytes = b.build();
        bytes[0] ^= 0xFF;

        let img = PlacesImage::parse(&bytes).expect("parse");
        assert!(!img.checksum_valid());
        assert_eq!(img.firmware_state(), FirmwareState::IntegrityCompromised);
    }

    #[test]
    fn state_concern_does_not_break_checksum() {
        let mut b = PlacesBuilder::new(0x1000_0100);
        b.set_version(1, 0, 0);
        let data: std::vec::Vec<u8> = (0..32u8).collect();
        b.add_segment(0x1200_0000, 0, &data, 32);

        let mut bytes = b.build();

        // Simulate the runtime writing a watchdog concern with detail=42.
        let state_offset = bytes.len() - STATE_OFFSET_FROM_END;
        let concern = FirmwareState::encode_concern(ConcernSource::Watchdog);
        write_u32(&mut bytes, state_offset, concern);

        let img = PlacesImage::parse(&bytes).expect("parse");
        assert!(img.checksum_valid());
        assert_eq!(img.firmware_state(), FirmwareState::WatchdogConcern);
    }

    #[test]
    fn select_prefers_bootable_over_concerned() {
        let build = |ver: (u16, u16, u16)| {
            let mut b = PlacesBuilder::new(0x1000);
            b.set_version(ver.0, ver.1, ver.2);
            b.add_segment(0x1200_0000, 0, &[0u8; 4], 4);
            b.build()
        };

        let bytes_a = build((1, 0, 0));
        let mut bytes_b = build((2, 0, 0));

        // B is higher version but has a concern — A should win.
        let off = bytes_b.len() - STATE_OFFSET_FROM_END;
        write_u32(
            &mut bytes_b,
            off,
            FirmwareState::encode_concern(ConcernSource::Kernel),
        );

        let a = PlacesImage::parse(&bytes_a).unwrap();
        let b = PlacesImage::parse(&bytes_b).unwrap();
        let (slot, reason) = select_firmware(&a, &b).unwrap();
        assert_eq!(slot, Slot::A);
        assert!(matches!(reason, SelectionReason::OtherConcerned(_)));
    }

    #[test]
    fn select_prefers_higher_version() {
        let build = |major, minor, patch| {
            let mut b = PlacesBuilder::new(0x1000);
            b.set_version(major, minor, patch);
            b.add_segment(0x1200_0000, 0, &[0u8; 4], 4);
            b.build()
        };

        let bytes_a = build(1, 0, 0);
        let bytes_b = build(1, 1, 0);
        let a = PlacesImage::parse(&bytes_a).unwrap();
        let b = PlacesImage::parse(&bytes_b).unwrap();
        assert_eq!(
            select_firmware(&a, &b).unwrap(),
            (Slot::B, SelectionReason::HigherVersion),
        );
    }

    #[test]
    fn select_falls_back_to_timestamp() {
        let build = |ts| {
            let mut b = PlacesBuilder::new(0x1000);
            b.set_version(1, 0, 0);
            b.set_flash_timestamp(ts);
            b.add_segment(0x1200_0000, 0, &[0u8; 4], 4);
            b.build()
        };

        let bytes_a = build(1000);
        let bytes_b = build(2000);
        let a = PlacesImage::parse(&bytes_a).unwrap();
        let b = PlacesImage::parse(&bytes_b).unwrap();
        assert_eq!(
            select_firmware(&a, &b).unwrap(),
            (Slot::B, SelectionReason::NewerFlash),
        );
    }

    #[test]
    fn select_tiebreak_is_a() {
        let build = || {
            let mut b = PlacesBuilder::new(0x1000);
            b.set_version(1, 0, 0);
            b.set_flash_timestamp(1000);
            b.add_segment(0x1200_0000, 0, &[0u8; 4], 4);
            b.build()
        };

        let bytes_a = build();
        let bytes_b = build();
        let a = PlacesImage::parse(&bytes_a).unwrap();
        let b = PlacesImage::parse(&bytes_b).unwrap();
        assert_eq!(
            select_firmware(&a, &b).unwrap(),
            (Slot::A, SelectionReason::Tiebreak),
        );
    }

    #[test]
    fn both_concerned_is_error() {
        let mut ba = PlacesBuilder::new(0x1000);
        ba.add_segment(0x1200_0000, 0, &[0u8; 4], 4);
        let mut bb = PlacesBuilder::new(0x1000);
        bb.add_segment(0x1200_0000, 0, &[0u8; 4], 4);

        let mut bytes_a = ba.build();
        let mut bytes_b = bb.build();

        let off_a = bytes_a.len() - STATE_OFFSET_FROM_END;
        let off_b = bytes_b.len() - STATE_OFFSET_FROM_END;
        write_u32(
            &mut bytes_a,
            off_a,
            FirmwareState::encode_concern(ConcernSource::Watchdog),
        );
        write_u32(
            &mut bytes_b,
            off_b,
            FirmwareState::encode_concern(ConcernSource::Runtime),
        );

        let a = PlacesImage::parse(&bytes_a).unwrap();
        let b = PlacesImage::parse(&bytes_b).unwrap();
        assert!(select_firmware(&a, &b).is_err());
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut bytes = std::vec![0u8; 24];
        bytes[20..24].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        assert!(matches!(
            PlacesImage::parse(&bytes),
            Err(ParseError::BadMagic)
        ));
    }
}
