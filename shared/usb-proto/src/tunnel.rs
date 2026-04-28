use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

/// Capacity of the data region — fits the largest wire format
/// (decoded frame + 2-byte CRC + 1-byte pad).
pub const TUNNEL_DATA_SIZE: usize = super::MAX_DECODED_FRAME + 3;

const FREE: u32 = 0;

/// Shared buffer for the IPC tunnel pipeline.
///
/// Lives in a shared memory region mapped into all three tunnel tasks
/// (transport, host_proxy, log). The lock serializes the two
/// transports against each other and tracks ownership as the buffer
/// passes through the pipeline.
///
/// Lock lifecycle (lock value = current owner's TaskId):
///   1. Transport acquires          (lock = transport)
///   2. Transport accumulates, stages, transfers to host_proxy
///                                  (lock = host_proxy)
///   3. Transport notifies host_proxy
///   4. host_proxy dispatches, encodes reply, transfers back
///                                  (lock = transport)
///   5. host_proxy calls deliver_reply
///   6. Transport reads reply, sends to wire, releases
///                                  (lock = 0)
///
/// Only the current owner may access `data` or `len`.
///
/// The lock stores the full Hubris `TaskId.0` as a u32. If the
/// holder was restarted (generation changed), the lock is stale and
/// can be wiped via `try_acquire_or_wipe`.
///
/// `repr(C)` so all tasks agree on field offsets.
#[repr(C)]
pub struct TunnelBuffer {
    lock: AtomicU32,
    len: UnsafeCell<u32>,
    data: UnsafeCell<[u8; TUNNEL_DATA_SIZE]>,
}

unsafe impl Sync for TunnelBuffer {}

impl TunnelBuffer {
    pub const fn new() -> Self {
        Self {
            lock: AtomicU32::new(FREE),
            len: UnsafeCell::new(0),
            data: UnsafeCell::new([0u8; TUNNEL_DATA_SIZE]),
        }
    }

    pub fn try_acquire(&self, tid: u32) -> bool {
        debug_assert!(tid != FREE);
        self.lock
            .compare_exchange(FREE, tid, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// Try to acquire. If held by a stale task (generation changed),
    /// wipe and re-acquire.
    pub fn try_acquire_or_wipe(
        &self,
        tid: u32,
        refresh: impl FnOnce(u32) -> u32,
    ) -> bool {
        if self.try_acquire(tid) {
            return true;
        }
        let current = self.lock.load(Ordering::Relaxed);
        if current == FREE {
            return self.try_acquire(tid);
        }
        let refreshed = refresh(current);
        if refreshed != current {
            if self
                .lock
                .compare_exchange(current, tid, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
        false
    }

    /// Transfer ownership to another task. Caller must be the current
    /// owner.
    pub fn transfer(&self, new_owner: u32) {
        debug_assert!(new_owner != FREE);
        self.lock.store(new_owner, Ordering::Release);
    }

    pub fn release(&self) {
        self.lock.store(FREE, Ordering::Release);
    }

    pub fn is_held(&self) -> bool {
        self.lock.load(Ordering::Relaxed) != FREE
    }

    pub fn holder(&self) -> u32 {
        self.lock.load(Ordering::Relaxed)
    }

    pub fn get_len(&self) -> u32 {
        unsafe { *self.len.get() }
    }

    /// # Safety
    /// Caller must be the current owner.
    pub unsafe fn set_len(&self, v: u32) {
        *self.len.get() = v;
    }

    /// # Safety
    /// Caller must be the current owner.
    pub unsafe fn data_ref(&self) -> &[u8; TUNNEL_DATA_SIZE] {
        &*self.data.get()
    }

    /// # Safety
    /// Caller must be the current owner.
    pub unsafe fn data_mut(&self) -> &mut [u8; TUNNEL_DATA_SIZE] {
        &mut *self.data.get()
    }
}
