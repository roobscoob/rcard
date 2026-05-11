//! Waker implementation that posts a notification bit to self.

use core::sync::atomic::{AtomicU32, Ordering};
use core::task::{RawWaker, RawWakerVTable, Waker};

use super::EXECUTOR_BIT;

#[repr(C)]
pub struct WakerData {
    pub(crate) ready_mask: *const AtomicU32,
    pub(crate) self_task_id: u16,
    pub(crate) task_index: u8,
}

impl WakerData {
    pub(crate) unsafe fn into_waker(&self) -> Waker {
        unsafe { Waker::from_raw(RawWaker::new(self as *const Self as *const (), &VTABLE)) }
    }
}

static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop_waker);

unsafe fn clone(data: *const ()) -> RawWaker {
    RawWaker::new(data, &VTABLE)
}

unsafe fn wake(data: *const ()) {
    unsafe { wake_by_ref(data) }
}

unsafe fn wake_by_ref(data: *const ()) {
    unsafe {
        let wd = &*(data as *const WakerData);
        (*wd.ready_mask).fetch_or(1 << wd.task_index, Ordering::Release);
        let tid = crate::kern::TaskId::gen0(wd.self_task_id);
        let _ = crate::kern::sys_post(tid, EXECUTOR_BIT);
    }
}

unsafe fn drop_waker(_data: *const ()) {}
