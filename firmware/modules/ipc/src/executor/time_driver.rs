//! Embassy-time driver backed by the Hubris kernel timer.
//!
//! `now()` returns kernel ticks (milliseconds). `schedule_wake()` stores
//! wakers and arms `sys_set_timer` for the earliest deadline. The
//! macro-generated run loop calls `expire_timers()` when TIMER_BIT fires.

use core::cell::UnsafeCell;
use core::task::Waker;

use embassy_time_driver::Driver;

use super::TIMER_BIT;

const MAX_ALARMS: usize = 8;

struct TimerEntry {
    deadline: u64,
    waker: Option<Waker>,
}

pub struct HubrisTimeDriver {
    entries: UnsafeCell<[TimerEntry; MAX_ALARMS]>,
}

unsafe impl Send for HubrisTimeDriver {}
unsafe impl Sync for HubrisTimeDriver {}

impl HubrisTimeDriver {
    const fn new() -> Self {
        const EMPTY: TimerEntry = TimerEntry {
            deadline: u64::MAX,
            waker: None,
        };
        Self {
            entries: UnsafeCell::new([EMPTY; MAX_ALARMS]),
        }
    }

    fn entries_mut(&self) -> &mut [TimerEntry; MAX_ALARMS] {
        unsafe { &mut *self.entries.get() }
    }

    fn arm_earliest(&self) {
        let entries = self.entries_mut();
        let earliest = entries
            .iter()
            .filter(|e| e.waker.is_some())
            .map(|e| e.deadline)
            .min();
        userlib::sys_set_timer(earliest, TIMER_BIT);
    }

    /// Wake all entries whose deadline has passed. Called from the run
    /// loop when TIMER_BIT fires.
    pub fn expire_timers(&self) {
        let now = userlib::sys_get_timer().now;
        let entries = self.entries_mut();
        for entry in entries.iter_mut() {
            if let Some(ref waker) = entry.waker {
                if now >= entry.deadline {
                    waker.wake_by_ref();
                    entry.waker = None;
                    entry.deadline = u64::MAX;
                }
            }
        }
        self.arm_earliest();
    }
}

impl embassy_time_driver::Driver for HubrisTimeDriver {
    fn now(&self) -> u64 {
        userlib::sys_get_timer().now
    }

    fn schedule_wake(&self, at: u64, waker: &Waker) {
        let entries = self.entries_mut();

        // Check if this waker is already registered — update deadline.
        for entry in entries.iter_mut() {
            if entry.waker.as_ref().is_some_and(|w| w.will_wake(waker)) {
                entry.deadline = at;
                self.arm_earliest();
                return;
            }
        }

        // Find a free slot.
        for entry in entries.iter_mut() {
            if entry.waker.is_none() {
                entry.deadline = at;
                entry.waker = Some(waker.clone());
                self.arm_earliest();
                return;
            }
        }

        // All slots full — replace the one with the latest deadline
        // (least urgent).
        let latest = entries
            .iter_mut()
            .max_by_key(|e| e.deadline);
        if let Some(entry) = latest {
            entry.deadline = at;
            entry.waker = Some(waker.clone());
            self.arm_earliest();
        }
    }
}

embassy_time_driver::time_driver_impl!(static DRIVER: HubrisTimeDriver = HubrisTimeDriver::new());

/// Returns the earliest deadline currently registered with the driver,
/// or `None` if no alarms are pending. Used by `arm_kernel_timer`.
pub fn earliest_pending() -> Option<u64> {
    DRIVER
        .entries_mut()
        .iter()
        .filter(|e| e.waker.is_some())
        .map(|e| e.deadline)
        .min()
}

/// Called from the macro-generated run loop when TIMER_BIT fires.
/// Expires all due timers and re-arms the kernel timer.
pub fn on_timer_tick() {
    DRIVER.expire_timers();
}

/// Register a sleep deadline with the time driver. Used by `Sleep`
/// futures that don't go through embassy-time.
pub fn schedule_sleep(at: u64, waker: &Waker) {
    Driver::schedule_wake(&DRIVER, at, waker);
}
