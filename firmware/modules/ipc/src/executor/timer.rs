//! Timer queue sharing the single kernel timer across multiple futures.

use core::cell::Cell;

use super::TIMER_BIT;

/// Manages per-task deadlines over the single kernel timer alarm.
///
/// The kernel provides exactly one timer per task. We track up to N
/// logical deadlines and always arm the hardware to the earliest one.
pub struct TimerQueue<const N: usize> {
    deadlines: Cell<[Option<u64>; N]>,
}

impl<const N: usize> TimerQueue<N> {
    pub const fn new() -> Self {
        Self {
            deadlines: Cell::new([None; N]),
        }
    }

    pub fn set(&self, task_idx: usize, deadline: u64) {
        let mut d = self.deadlines.get();
        d[task_idx] = Some(deadline);
        self.deadlines.set(d);
    }

    pub fn cancel(&self, task_idx: usize) {
        let mut d = self.deadlines.get();
        d[task_idx] = None;
        self.deadlines.set(d);
    }

    /// Expire all deadlines <= `now`. Returns a bitmask of task indices
    /// whose timers fired.
    pub fn expire(&self, now: u64) -> u32 {
        let mut d = self.deadlines.get();
        let mut fired: u32 = 0;
        for (i, slot) in d.iter_mut().enumerate() {
            if let Some(deadline) = *slot {
                if now >= deadline {
                    fired |= 1 << i;
                    *slot = None;
                }
            }
        }
        self.deadlines.set(d);
        fired
    }

    /// Arm the kernel timer to the earliest pending deadline.
    /// If no deadlines are pending, disarms the timer.
    pub fn arm_kernel_timer(&self) {
        let d = self.deadlines.get();
        let earliest = d.iter().filter_map(|s| *s).min();
        userlib::sys_set_timer(earliest, TIMER_BIT);
    }
}

/// Return the current kernel tick count.
#[inline]
pub fn now() -> u64 {
    userlib::sys_get_timer().now
}

/// A future that completes after `ms` milliseconds.
///
/// Must be polled from a task whose index is registered with the
/// executor. The timer queue and ready mask are accessed through
/// the executor context established by the macro-generated run loop.
pub struct Sleep {
    deadline: u64,
    registered: bool,
}

impl Sleep {
    pub fn new(ms: u64) -> Self {
        Self {
            deadline: now() + ms,
            registered: false,
        }
    }

    pub fn until(deadline: u64) -> Self {
        Self {
            deadline,
            registered: false,
        }
    }
}

impl core::future::Future for Sleep {
    type Output = ();

    fn poll(
        mut self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<()> {
        if now() >= self.deadline {
            return core::task::Poll::Ready(());
        }
        // Register/update the waker with the embassy-time driver.
        // The driver stores (deadline, waker) and arms the kernel timer.
        super::time_driver::schedule_sleep(self.deadline, cx.waker());
        self.registered = true;
        core::task::Poll::Pending
    }
}

/// Convenience: sleep for `ms` kernel ticks.
pub fn sleep_ms(ms: u64) -> Sleep {
    Sleep::new(ms)
}

/// Convenience: sleep until an absolute deadline.
pub fn sleep_until(deadline: u64) -> Sleep {
    Sleep::until(deadline)
}

