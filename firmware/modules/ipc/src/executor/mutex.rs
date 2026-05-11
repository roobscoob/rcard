//! Embassy-sync compatibility for single-threaded Hubris tasks.

/// No-op critical-section for the `critical-section` crate.
///
/// Hubris tasks are single-threaded with no user-mode preemption.
/// IRQ handlers run in the kernel, not in user code.
struct HubrisCriticalSection;
critical_section::set_impl!(HubrisCriticalSection);

unsafe impl critical_section::Impl for HubrisCriticalSection {
    unsafe fn acquire() -> critical_section::RawRestoreState {
        // No-op: single-threaded, no user-mode interrupts.
    }

    unsafe fn release(_: critical_section::RawRestoreState) {
        // No-op.
    }
}

/// No-op mutex for embassy-sync primitives.
///
/// Safety: Hubris tasks are single-threaded. IRQ handlers run in the
/// kernel and post notification bits — they never execute user code
/// directly. The IPC server dispatch is non-reentrant.
pub struct HubrisMutex;

unsafe impl embassy_sync::blocking_mutex::raw::RawMutex for HubrisMutex {
    const INIT: Self = Self;

    fn lock<R>(&self, f: impl FnOnce() -> R) -> R {
        f()
    }
}

pub type Signal<T> = embassy_sync::signal::Signal<HubrisMutex, T>;
pub type Channel<T, const N: usize> = embassy_sync::channel::Channel<HubrisMutex, T, N>;
