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

/// Single-threaded global state with runtime reentrance detection.
///
/// Wraps a `T` in an `UnsafeCell` and provides closure-based access via
/// [`with`](GlobalState::with). If `with` is called while a previous `with`
/// is still on the stack (reentrant access), the call panics — preventing the
/// aliased `&mut` references that `static mut` silently allows.
///
/// # Const-initialized state
///
/// ```ignore
/// static STATE: GlobalState<MyState> = GlobalState::new(MyState::new());
/// STATE.with(|s| s.do_something());
/// ```
///
/// # Late-initialized state
///
/// Compose with [`OnceCell`] for state that cannot be created at `const` time:
///
/// ```ignore
/// static STATE: OnceCell<GlobalState<MyState>> = OnceCell::new();
/// // In main():
/// STATE.set(GlobalState::new(MyState::init())).ok();
/// // Later:
/// STATE.get().expect("not initialized").with(|s| s.do_something());
/// ```
///
/// # Safety
///
/// Sound only in single-threaded contexts (Hubris tasks). The `Sync` impl
/// exists solely so `GlobalState` can be placed in a `static`.
pub struct GlobalState<T> {
    inner: UnsafeCell<T>,
    borrowed: AtomicBool,
}

// SAFETY: Hubris tasks are single-threaded. The `borrowed` flag prevents
// aliased `&mut` within a single thread.
unsafe impl<T> Sync for GlobalState<T> {}

impl<T> GlobalState<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: UnsafeCell::new(value),
            borrowed: AtomicBool::new(false),
        }
    }

    /// Access the state exclusively through a closure.
    ///
    /// Panics if called while another `with` is still on the stack.
    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        if self.borrowed.swap(true, Ordering::Acquire) {
            panic!();
        }
        let result = f(unsafe { &mut *self.inner.get() });
        self.borrowed.store(false, Ordering::Release);
        result
    }
}
