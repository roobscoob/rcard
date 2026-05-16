#![no_std]

use rcard_log::Format;

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum HibernateError {
    NoMatchingRegion = 0,
    AlreadyHibernated = 1,
    GenerationOverflow = 2,
    ProtectedMemory = 3,
    SupervisorRejected = 4,
}

#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum RestoreError {
    InvalidToken = 0,
    SupervisorRejected = 1,
}

#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum RegionIoError {
    NotHibernated = 0,
    SupervisorRejected = 1,
}

/// RAII guard for a hibernated memory region.
///
/// Created by [`RegionHibernation::hibernate`]. While this resource
/// exists, all tasks whose MPU regions overlap the hibernated range
/// are suspended.
///
/// - Call [`restore`](RegionHibernation::restore) to resume tasks
///   (contents preserved).
/// - Dropping the resource without calling `restore` resumes tasks
///   with `contents_lost = true`, faulting any that depended on the
///   region's data.
#[ipc::resource(arena_size = 4, kind = 0x0D)]
pub trait RegionHibernation {
    /// Hibernate a memory region, suspending all tasks whose MPU
    /// regions overlap `[base, base+size)`.
    ///
    /// The caller (and this sysmodule) must be RAM-resident to
    /// avoid deadlock.
    #[constructor]
    fn hibernate(base: u32, size: u32) -> Result<Self, HibernateError>;

    /// Restore the hibernated region. Tasks resume normally; the
    /// region's contents are assumed to be intact.
    #[message]
    fn restore(&mut self) -> Result<(), RestoreError>;

    /// Read bytes from the hibernated region starting at `address`.
    /// Returns the number of bytes copied.
    #[message]
    fn read(&mut self, address: u32, #[lease] buf: &mut [u8]) -> Result<u32, RegionIoError>;

    /// Write bytes into the hibernated region starting at `address`.
    /// Returns the number of bytes copied.
    #[message]
    fn write(&mut self, address: u32, #[lease] data: &[u8]) -> Result<u32, RegionIoError>;
}
