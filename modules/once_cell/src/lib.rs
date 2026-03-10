#![no_std]

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

pub struct OnceCell<T> {
    initialized: AtomicBool,
    value: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: This is a single-threaded OnceCell. We implement Sync so it can be
// used in `static` items, which is sound in single-threaded (no_std) contexts
// where no concurrent access occurs.
unsafe impl<T> Sync for OnceCell<T> {}

impl<T> OnceCell<T> {
    pub const fn new() -> Self {
        Self {
            initialized: AtomicBool::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    pub fn get(&self) -> Option<&T> {
        if self.initialized.load(Ordering::Acquire) {
            // SAFETY: Value is initialized and we only hand out shared refs.
            Some(unsafe { (*self.value.get()).assume_init_ref() })
        } else {
            None
        }
    }

    pub fn set(&self, value: T) -> Result<(), T> {
        if self.initialized.load(Ordering::Acquire) {
            return Err(value);
        }
        // SAFETY: Single-threaded; no concurrent access.
        unsafe {
            (*self.value.get()).write(value);
        }
        self.initialized.store(true, Ordering::Release);
        Ok(())
    }
}
