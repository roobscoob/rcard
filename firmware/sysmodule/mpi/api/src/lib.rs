#![no_std]

pub mod sfdp;

#[derive(
    Debug,
    Clone,
    Copy,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum MpiOpenError {
    InvalidIndex = 0,
    AlreadyOpen = 1,
    /// Chip did not respond to RDID after reset — JEDEC came back as
    /// all-zero or all-ones, or decoded capacity is nonsense. Usually
    /// wiring, CS stuck, or chip dead.
    ChipNotResponding = 2,
    /// The initial JEDEC read after reset failed — either the peripheral
    /// never signaled TCF, the RX FIFO never filled, or the TX FIFO
    /// never drained. The specific `MpiOperationError` variant is logged
    /// at `error` level by the driver; this enum collapses them because
    /// `open()`'s failure taxonomy doesn't benefit from distinguishing
    /// peripheral-side from chip-side timing for this one call.
    JedecReadFailed = 3,
    /// SFDP read/parse failed — either the chip doesn't speak SFDP,
    /// the signature was wrong, BFPT was missing, or the density field
    /// was invalid. Capacity is derived from BFPT DWORD 2, so without
    /// a valid SFDP we can't bounds-check subsequent operations and
    /// refuse to open rather than run unbounded.
    SfdpUnavailable = 4,
    /// `EN4B` (enter-4-byte-mode) or `EX4B` (exit-4-byte-mode) failed.
    /// Continuing would use the wrong address width for every
    /// subsequent op, corrupting reads/writes/erases silently, so
    /// open() refuses rather than press on.
    AddressModeSwitchFailed = 5,
}

/// Runtime failures during normal MPI operations (read/write/erase/...).
/// Split by cause so the caller can tell an MPI-side hang from a
/// chip-side silent-treatment from a caller bug.
#[derive(
    Debug,
    Clone,
    Copy,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum MpiOperationError {
    /// MPI never signaled TCF. MPI-side fault: CR.EN=0, clock gated,
    /// peripheral wedged. Chip probably isn't even involved.
    TransferTimeout = 0,
    /// RX FIFO never filled with expected data. Chip-side: in QPI when
    /// we sent SPI, in DPD, dead, or wrong pins.
    RxFifoTimeout = 1,
    /// TX FIFO never drained. MPI-side backpressure / stall.
    TxFifoTimeout = 2,
    /// WIP bit never cleared. Either chip is wedged mid-op, or the
    /// operation is legitimately running longer than MAX_WIP_POLLS.
    WipTimeout = 3,
    /// Sent CMD_WRITE_ENABLE but WEL did not latch in SR1. Usually WP#
    /// pin held low, SRP bits locking status writes, or chip ignoring
    /// the command entirely.
    WriteEnableDidNotLatch = 4,
    /// Address (or address + length) falls outside the chip's capacity.
    AddressOutOfRange = 5,
    /// Read buffer exceeds the driver's internal page-sized staging
    /// buffer. Split the read caller-side.
    LengthTooLarge = 6,
    /// Address is not aligned to the required erase granularity (4 KB).
    InvalidAddressAlignment = 7,
    /// Length is not aligned to the required erase granularity (4 KB).
    InvalidLengthAlignment = 8,
}

/// Number of data lines used for a transfer phase.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum LineMode {
    /// Phase is skipped entirely.
    None = 0,
    /// Single data line (standard SPI).
    Single = 1,
    /// Dual data lines.
    Dual = 2,
    /// Quad data lines.
    Quad = 3,
    /// Quad data lines, DDR (double data rate).
    QuadDdr = 7,
}

/// Number of address bytes sent per transaction.
#[derive(
    Debug,
    Clone,
    Copy,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum AddrSize {
    /// 1-byte address (8-bit).
    OneByte = 0,
    /// 2-byte address (16-bit).
    TwoBytes = 1,
    /// 3-byte address (24-bit, covers up to 16MB).
    ThreeBytes = 2,
    /// 4-byte address (32-bit, for >16MB devices).
    FourBytes = 3,
}

/// Output clock polarity.
#[derive(
    Debug,
    Clone,
    Copy,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum ClockPolarity {
    /// Normal clock output (SCKINV = 0).
    Normal = 0,
    /// Inverted clock output (SCKINV = 1, hardware reset default).
    Inverted = 1,
}

/// Caller preference for the line-mode the driver should use for reads.
/// The driver resolves this against BFPT's fast-read advertisements at
/// `open()`; if the preferred mode isn't supported by the chip the
/// driver falls back to the next-best mode it actually advertises.
#[derive(
    Debug,
    Clone,
    Copy,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum ModePreference {
    /// Single-line SPI (1-1-1). Slowest, no chip-side setup required.
    /// Use for bring-up, or chips whose QE-bit location isn't known.
    Single = 0,
    /// The fastest mode BFPT advertises. Prefers 1-4-4 → 1-1-4 → 1-2-2
    /// → 1-1-2 → 1-1-1 fast read. Quad modes require the chip's QE
    /// bit to be set; the driver reads BFPT's QER advertisement and
    /// writes the bit if needed.
    ///
    /// Caveat: the driver drives `ABR1 = 0xFF` during the mode-byte
    /// phase of fast-IO reads, which is the "not-CRM" pattern on
    /// Winbond / GD25 / Macronix. Other vendors (Cypress, ISSI, some
    /// EON) use a different CRM-exit encoding; on untested vendors a
    /// quad-IO read may leave the chip in continuous-read mode. Use
    /// `Single` during first bring-up on a new vendor.
    Fastest = 1,
}

/// Hardware-level configuration for an MPI instance. Only the
/// peripheral-side knobs that SFDP cannot advertise live here —
/// addressing width, line modes, and dummy-cycle counts are derived
/// from the chip's own SFDP parameter tables at `open()`.
#[derive(
    Debug,
    Clone,
    Copy,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(C, packed)]
pub struct MpiConfig {
    /// Clock prescaler divider (0 or 1 = no division, n = FCLK/n).
    /// Depends on the MCU clock tree, not on the chip.
    pub prescaler: u8,
    /// Output clock polarity. The SF32LB52 MPI peripheral's power-on
    /// default is SCKINV=1; using the opposite polarity samples MISO
    /// on the wrong edge.
    pub clock_polarity: ClockPolarity,
    /// Line-mode preference for reads — resolved against BFPT fast-read
    /// advertisements at `open()`, with graceful fallback.
    pub preferred_mode: ModePreference,
}

/// JEDEC manufacturer + device ID.
#[derive(
    Debug,
    Clone,
    Copy,
    zerocopy::FromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(C, packed)]
pub struct JedecId {
    manufacturer: u8,
    memory_type: u8,
    capacity: u8,
}

impl JedecId {
    pub fn new(manufacturer: u8, memory_type: u8, capacity: u8) -> Self {
        Self {
            manufacturer,
            memory_type,
            capacity,
        }
    }

    pub fn zero() -> Self {
        Self::new(0, 0, 0)
    }

    pub fn erased() -> Self {
        Self::new(0xFF, 0xFF, 0xFF)
    }

    pub fn is_responding(&self) -> bool {
        *self != Self::zero() && *self != Self::erased()
    }
}

impl PartialEq for JedecId {
    fn eq(&self, other: &Self) -> bool {
        self.manufacturer == other.manufacturer
            && self.memory_type == other.memory_type
            && self.capacity == other.capacity
    }
}

impl Eq for JedecId {}

/// SFDP global header. Returned by `read_sfdp` so callers can
/// sanity-check revision and discover how many parameter headers exist.
/// `#[repr(C)]` (not packed) — see [`ParameterHeader`] for rationale.
#[derive(
    Debug,
    Clone,
    Copy,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(C)]
pub struct SfdpHeader {
    pub major: u8,
    pub minor: u8,
    /// Number of parameter headers (1..=256). Stored as a real count,
    /// not the on-wire `n - 1` encoding.
    pub nph: u16,
    pub access_protocol: u8,
}

/// Failure modes for `read_sfdp`. Currently only one variant — open()
/// already validates SFDP and refuses to construct the resource if it's
/// unusable, so by the time read_sfdp is called this should never fire.
/// Kept as a Result for forward extensibility without a breaking change.
#[derive(
    Debug,
    Clone,
    Copy,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum ReadSfdpError {
    /// SFDP state was never populated at open() — defensive only,
    /// shouldn't be reachable in practice since open() returns
    /// `MpiOpenError::SfdpUnavailable` on the same condition.
    SfdpUnavailable = 0,
}

/// Failure modes for `read_parameter`. Distinct from `SfdpError` because
/// this method takes a lease and can fail for lease-specific reasons
/// that SFDP parsing can't.
#[derive(
    Debug,
    Clone,
    Copy,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum ReadParameterError {
    /// Chip's SFDP was unusable at `open()` (bad signature, no parameter
    /// headers, etc.), so no parameter table is reachable.
    SfdpUnavailable = 0,
    /// MPI never signaled TCF on the parameter body read.
    TransferTimeout = 1,
    /// RX FIFO never filled on the parameter body read.
    RxFifoTimeout = 2,
    /// TX FIFO never drained on the parameter body read.
    TxFifoTimeout = 3,
}

/// 16-bit SFDP parameter-table ID. MSB 0xFF = JEDEC-assigned; MSB < 0xFF
/// = JEP106 vendor ID for vendor-specific tables.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(transparent)]
pub struct ParameterId(pub u16);

impl ParameterId {
    /// Basic Flash Parameter Table — mandatory first entry on any
    /// SFDP-compliant SPI NOR.
    pub const BFPT: Self = Self(0xFF00);
    /// Status/Control/Configuration Register map.
    pub const SCCR: Self = Self(0xFF84);
    /// 4-Byte Address Instruction Table.
    pub const FOUR_BAIT: Self = Self(0xFF81);
    /// Quad Command Sequences.
    pub const QUAD_CMD: Self = Self(0xFF87);
}

/// One SFDP parameter header — 8 bytes on-wire, decoded. The in-memory
/// layout is `#[repr(C)]` (not packed) because consumers access fields
/// like `id` (u16) and `pointer` (u32) directly; packed-struct field
/// access through a reference is UB on aligned-by-default types. The
/// driver hand-encodes/decodes the 8-byte wire format at the SFDP read
/// boundary.
#[derive(
    Debug,
    Clone,
    Copy,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(C)]
pub struct ParameterHeader {
    pub id: ParameterId,
    pub major: u8,
    pub minor: u8,
    /// Length of the parameter table body in 32-bit DWORDs.
    pub length_dwords: u8,
    /// 24-bit SFDP-space address of the table body. Upper byte is zero.
    pub pointer: u32,
}

/// Metadata returned by `read_parameter`.
#[derive(
    Debug,
    Clone,
    Copy,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
pub struct ParameterMetadata {
    pub header: ParameterHeader,
    /// Total number of entries with this ID in the chip's SFDP.
    /// Callers can iterate `0..count` to walk duplicates.
    pub count: u8,
}

#[ipc::resource(arena_size = 2, kind = 0x08)]
pub trait Mpi {
    /// Open an MPI instance by index (1 or 2) with the given config.
    #[constructor]
    fn open(index: u8, config: MpiConfig) -> Result<Self, MpiOpenError>;

    /// Read the JEDEC ID (command 0x9F).
    #[message]
    fn read_jedec_id(&mut self) -> Result<JedecId, MpiOperationError>;

    /// Read the cached Serial Flash Discoverable Parameters table of
    /// contents (JESD216). Returns the SFDP global header (revision +
    /// parameter-header count) and writes the parameter-header table
    /// — `nph * 8` bytes in on-wire format — into `lease`. Caller
    /// parses each 8-byte slot into a [`ParameterHeader`].
    ///
    /// SFDP is read once at `open()` and cached, so this is a memcpy,
    /// not a flash transaction. To fetch a specific parameter table's
    /// body, call [`read_parameter`].
    ///
    /// An empty lease is a valid probe: the header is still returned
    /// and no copy happens.
    #[message]
    fn read_sfdp(
        &mut self,
        #[lease] lease: &mut [u8],
    ) -> Result<SfdpHeader, ReadSfdpError>;

    /// Read data starting at `address` into the lease buffer. Buffer
    /// must be ≤ the driver's page-sized staging buffer; callers with
    /// larger reads should chunk.
    #[message]
    fn read(&mut self, address: u32, #[lease] buf: &mut [u8]) -> Result<(), MpiOperationError>;

    /// Write data starting at `address` from the lease buffer.
    /// Caller is responsible for ensuring the region is erased first.
    /// Handles page-boundary splitting internally.
    #[message]
    fn write(&mut self, address: u32, #[lease] data: &[u8]) -> Result<(), MpiOperationError>;

    /// Erase a region starting at `address` for `length` bytes.
    /// Both `address` and `length` must be 4KB-aligned.
    /// Automatically selects 64KB, 32KB, or 4KB erase commands for efficiency.
    #[message]
    fn erase(&mut self, address: u32, length: u32) -> Result<(), MpiOperationError>;

    /// Erase the entire chip.
    #[message]
    fn erase_chip(&mut self) -> Result<(), MpiOperationError>;

    /// Read the body of the SFDP parameter table matching `id` at
    /// occurrence `index`. Writes up to `lease.len()` bytes from the
    /// body into `lease` (truncating silently if the table is larger).
    ///
    /// Semantics of the return value:
    /// - `Ok(Some(metadata))` — parameter found; up to `lease.len()`
    ///   bytes written; caller can size the next call via
    ///   `metadata.header.length_dwords * 4`.
    /// - `Ok(None)` — `id` not present on this chip, or
    ///   `index >= count(id)` (caller past the end of the duplicates).
    /// - `Err(_)` — SFDP was unusable at `open()`, or the body read
    ///   itself failed on the hardware.
    ///
    /// An empty lease (`lease.len() == 0`) is a valid probe: metadata
    /// is returned if the parameter exists, and no body read fires.
    #[message]
    fn read_parameter(
        &mut self,
        id: ParameterId,
        index: u8,
        #[lease] lease: &mut [u8],
    ) -> Result<Option<ParameterMetadata>, ReadParameterError>;
}
