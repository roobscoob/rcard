use crate::LogLevel;

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
pub struct LogMetadata {
    pub level: LogLevel,
    /// Monotonic kernel ticks since boot (from GET_TIMER syscall).
    pub timestamp: u64,
    pub source: u16,
    pub generation: u16,
    pub log_id: u64,
    pub log_species: u64,
}
