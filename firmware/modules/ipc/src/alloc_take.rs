use core::mem::MaybeUninit;
use core::ops::Not;
use core::sync::atomic::{AtomicBool, Ordering};

/// Take a memory-mapped allocation at `base`, returning `&'static mut MaybeUninit<T>`.
/// Panics if called twice.
///
/// Called by generated code from `ipc::allocation!()`. Not user-facing.
pub fn take<T>(taken: &AtomicBool, base: usize) -> Option<&'static mut MaybeUninit<T>> {
    taken
        .swap(true, Ordering::Relaxed)
        .not()
        .then(|| unsafe { &mut *(base as *mut MaybeUninit<T>) })
}
