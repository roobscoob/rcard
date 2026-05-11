//! Cooperative async executor for Hubris IPC tasks.
//!
//! Integrates with `sys_recv_open` by using dedicated notification bits
//! to wake the executor when futures become ready or timers expire.
//! The macro-generated run loop polls futures inline — no type erasure.

use core::sync::atomic::{AtomicU32, Ordering};
use core::task::Waker;

pub mod mutex;
pub mod time_driver;
pub mod timer;
pub mod waker;

pub use mutex::{Channel, HubrisMutex, Signal};
pub use timer::{Sleep, TimerQueue, now, sleep_ms, sleep_until};

/// Notification bit posted by `Waker::wake()` to unblock `sys_recv_open`.
pub const EXECUTOR_BIT: u32 = 1 << 29;

/// Notification bit posted by the kernel timer on deadline expiry.
pub const TIMER_BIT: u32 = 1 << 30;

/// Per-task executor state. `N` is the number of async futures.
///
/// The macro generates an `ExecutorState<N>` as a local in main and
/// uses it to track which futures need polling and to produce wakers.
pub struct ExecutorState<const N: usize> {
    pub ready_mask: AtomicU32,
    pub timer_queue: TimerQueue<N>,
    waker_data: [waker::WakerData; N],
}

impl<const N: usize> ExecutorState<N> {
    /// Create a new executor state. `self_task_id` is the task's own
    /// index (from `generated::tasks`), used for `sys_post` in wakers.
    pub fn new(self_task_id: u16) -> Self {
        let ready_mask = AtomicU32::new(0);
        let ready_mask_ptr: *const AtomicU32 = &ready_mask;

        let mut waker_data: [waker::WakerData; N] = unsafe { core::mem::zeroed() };
        for (i, wd) in waker_data.iter_mut().enumerate() {
            wd.ready_mask = ready_mask_ptr;
            wd.self_task_id = self_task_id;
            wd.task_index = i as u8;
        }

        Self {
            ready_mask,
            timer_queue: TimerQueue::new(),
            waker_data,
        }
    }

    /// Fixup waker data pointers after the struct has been moved to its
    /// final location. Must be called once after construction before any
    /// wakers are created.
    pub fn fixup_waker_pointers(&mut self) {
        let ptr: *const AtomicU32 = &self.ready_mask;
        for wd in self.waker_data.iter_mut() {
            wd.ready_mask = ptr;
        }
    }

    /// Mark all tasks as ready so the initial poll runs them once.
    pub fn mark_all_ready(&self) {
        let mask = if N >= 32 { u32::MAX } else { (1u32 << N) - 1 };
        self.ready_mask.store(mask, Ordering::Release);
    }

    /// Create a `Waker` for async task at `index`.
    ///
    /// # Safety
    /// The returned waker references `self` by raw pointer. Caller must
    /// ensure `self` outlives all uses of the waker (guaranteed when
    /// ExecutorState is a local in `main() -> !`).
    pub fn make_waker(&self, index: usize) -> Waker {
        unsafe { self.waker_data[index].into_waker() }
    }
}
