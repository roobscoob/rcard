#![no_std]

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
}

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
pub enum EraseError {
    InvalidAddressAlignment = 0,
    InvalidLengthAlignment = 1,
}

/// Number of data lines used for a transfer phase.
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

/// Configuration for an MPI instance.
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
    pub prescaler: u8,
    /// Address size.
    pub addr_size: AddrSize,
    /// Line mode for the instruction phase.
    pub imode: LineMode,
    /// Line mode for the address phase.
    pub admode: LineMode,
    /// Line mode for the data phase.
    pub dmode: LineMode,
    /// Number of dummy cycles for reads (0..31).
    pub read_dummy_cycles: u8,
    /// Output clock polarity.
    pub clock_polarity: ClockPolarity,
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
    pub manufacturer: u8,
    pub memory_type: u8,
    pub capacity: u8,
}

#[ipc::resource(arena_size = 2, kind = 0x08)]
pub trait Mpi {
    /// Open an MPI instance by index (1 or 2) with the given config.
    #[constructor]
    fn open(index: u8, config: MpiConfig) -> Result<Self, MpiOpenError>;

    /// Read the JEDEC ID (command 0x9F).
    #[message]
    fn read_jedec_id(&mut self) -> JedecId;

    /// Read data starting at `address` into the lease buffer.
    #[message]
    fn read(&mut self, address: u32, #[lease] buf: &mut [u8]);

    /// Write data starting at `address` from the lease buffer.
    /// Caller is responsible for ensuring the region is erased first.
    /// Handles page-boundary splitting internally.
    #[message]
    fn write(&mut self, address: u32, #[lease] data: &[u8]);

    /// Erase a region starting at `address` for `length` bytes.
    /// Both `address` and `length` must be 4KB-aligned.
    /// Automatically selects 64KB, 32KB, or 4KB erase commands for efficiency.
    #[message]
    fn erase(&mut self, address: u32, length: u32) -> Result<(), EraseError>;

    /// Erase the entire chip.
    #[message]
    fn erase_chip(&mut self);
}
