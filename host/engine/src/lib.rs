pub mod logs;

use logs::Logs;

/// A live connection to a device (real or emulated).
///
/// Each backend type has its own constructor that returns `impl Backend`:
/// - `Emulator::start(tfw) -> impl Backend`
/// - `UnmanagedDebug::connect(usart1, usart2) -> impl Backend`
/// - (future) `ManagedUsb::connect(...) -> impl Backend`
/// - (future) `ManagedBle::connect(...) -> impl Backend`
///
/// Construction is RAII — a Backend represents a live session. Drop ends it.
pub trait Backend: Send + Sync {
    /// Structured + hypervisor log streams. Every backend supports this.
    fn logs(&self) -> &dyn Logs;
}
