use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

/// Take a memory-mapped allocation at `base`, returning `&'static mut MaybeUninit<T>`.
/// Panics if called twice.
///
/// Called by generated code from `ipc::allocation!()`. Not user-facing.
pub fn take<T>(taken: &AtomicBool, base: usize) -> &'static mut MaybeUninit<T> {
    if taken.swap(true, Ordering::Relaxed) {
        panic!();
    }
    unsafe { &mut *(base as *mut MaybeUninit<T>) }
}
