#![no_std]

#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(u8)]
pub enum StorageErrorKind {
    OutOfRange = 0,
    Device = 1,
    Alignment = 2,
}

#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(C, packed)]
pub struct StorageError {
    pub kind: StorageErrorKind,
    pub device_code: u16,
}

impl StorageError {
    pub fn out_of_range() -> Self {
        Self {
            kind: StorageErrorKind::OutOfRange,
            device_code: 0,
        }
    }
    pub fn device(code: u16) -> Self {
        Self {
            kind: StorageErrorKind::Device,
            device_code: code,
        }
    }
    pub fn alignment() -> Self {
        Self {
            kind: StorageErrorKind::Alignment,
            device_code: 0,
        }
    }
}

/// Storage device geometry — erase/program/read granularities.
#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::FromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(C, packed)]
pub struct Geometry {
    /// Total usable size in bytes.
    pub total_size: u32,
    /// Minimum erase unit in bytes (e.g. 4096 for NOR flash).
    pub erase_size: u32,
    /// Minimum program unit in bytes (e.g. 256 for NOR flash page).
    pub program_size: u32,
    /// Minimum read unit in bytes (e.g. 1 for NOR flash).
    pub read_size: u32,
}

/// Byte-addressed storage interface.
///
/// Two write paths:
/// - `write()` = safe erase-then-program (for simple callers).
/// - `erase()` + `program()` = split path for callers that manage
///   their own sequencing (e.g. littlefs).
#[ipc::interface(kind = 0x10)]
pub trait Storage {
    /// Read `buf.len()` bytes starting at `offset`.
    #[message]
    fn read(&self, offset: u32, #[lease] buf: &mut [u8]) -> Result<(), StorageError>;

    /// Erase-then-program. Both `offset` and `buf.len()` must be
    /// erase-size-aligned.
    #[message]
    fn write(&self, offset: u32, #[lease] buf: &[u8]) -> Result<(), StorageError>;

    /// Erase a region. Both `offset` and `len` must be erase-size-aligned.
    #[message]
    fn erase(&self, offset: u32, len: u32) -> Result<(), StorageError>;

    /// Program (write without erase). Caller guarantees the region was
    /// previously erased.
    #[message]
    fn program(&self, offset: u32, #[lease] buf: &[u8]) -> Result<(), StorageError>;

    /// Query device geometry (sizes and granularities).
    #[message]
    fn geometry(&self) -> Geometry;
}
