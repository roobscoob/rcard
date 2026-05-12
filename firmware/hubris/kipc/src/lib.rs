//! API for the kernel's `kipc` interface. This should only be used from
//! supervisor tasks.

#![no_std]

use core::{num::NonZeroUsize, sync::atomic::Ordering};

use abi::Kipcnum;
pub use abi::{HibernationEventId, RestoreMode, TaskState};
use userlib::Lease;

/// Reads the scheduling/fault status of the task with the given index.
pub fn read_task_status(task_index: usize) -> TaskState {
    let mut buf = [0u8; size_of::<TaskState>()];
    buf[..4].copy_from_slice(&(task_index as u32).to_le_bytes());
    let (_, len) = userlib::sys_send_to_kernel(
        Kipcnum::ReadTaskStatus as u16,
        &mut buf,
        4,
        &mut [],
    );
    match ssmarshal::deserialize(&buf[..len]) {
        Ok((state, _)) => state,
        Err(_) => panic!(),
    }
}

/// Scans tasks from `task_index` looking for a task that has failed.
pub fn find_faulted_task(task_index: usize) -> Option<NonZeroUsize> {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&(task_index as u32).to_le_bytes());
    let (_, _len) = userlib::sys_send_to_kernel(
        Kipcnum::FindFaultedTask as u16,
        &mut buf,
        4,
        &mut [],
    );
    let i = u32::from_le_bytes(buf);
    NonZeroUsize::new(i as usize)
}

/// Requests that the task at a given index be reinitialized and optionally started.
pub fn reinitialize_task(task_index: usize, new_state: NewState) {
    let mut buf = [0u8; 5];
    buf[..4].copy_from_slice(&(task_index as u32).to_le_bytes());
    buf[4] = new_state as u8;

    let _ = userlib::sys_send_to_kernel(
        Kipcnum::ReinitTask as u16,
        &mut buf,
        5,
        &mut [],
    );
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NewState {
    Halted = 0,
    Runnable = 1,
}

/// Reads the panic message from a faulted task into `buf`.
///
/// Returns `Ok(len)` with the number of bytes written,
/// or `Err(rc)` if the task hasn't panicked or its panic buffer is invalid.
pub fn read_panic_message(task_index: usize, buf: &mut [u8]) -> Result<usize, u32> {
    buf[..4].copy_from_slice(&(task_index as u32).to_le_bytes());
    let (rc, len) = userlib::sys_send_to_kernel(
        Kipcnum::ReadPanicMessage as u16,
        buf,
        4,
        &mut [],
    );
    if rc == userlib::ResponseCode::SUCCESS {
        Ok(len)
    } else {
        Err(rc.0)
    }
}

pub fn reset() -> ! {
    userlib::sys_send_to_kernel(Kipcnum::Reset as u16, &mut [], 0, &mut []);
    loop {
        core::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}

/// Asks the kernel to hibernate the memory region `(base, size)`. The
/// region must exactly match one of the predefined hibernation regions.
///
/// Returns the `HibernationEventId` that must later be presented to
/// [`restore_region`] to bring the region back. Errors are surfaced as
/// the raw `HibernateRegionMessageError` discriminant.
pub fn hibernate_region(base: u32, size: u32) -> Result<HibernationEventId, u32> {
    // ssmarshal encodes (u32, u32) as 8 bytes; the response is a single
    // u32 inside `HibernationEventId`.
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&base.to_le_bytes());
    buf[4..].copy_from_slice(&size.to_le_bytes());

    let (rc, len) = userlib::sys_send_to_kernel(
        Kipcnum::HibernateRegion as u16,
        &mut buf,
        8,
        &mut [],
    );
    if rc != userlib::ResponseCode::SUCCESS {
        return Err(rc.0);
    }
    match ssmarshal::deserialize::<HibernationEventId>(&buf[..len]) {
        Ok((id, _)) => Ok(id),
        Err(_) => Err(u32::MAX),
    }
}

/// Reads `len` bytes out of an active hibernated region starting at
/// `addr` into `dest`. The kernel uses a writable lease at lease index 0
/// to write the data back out to the supervisor.
///
/// Returns the number of bytes actually copied. The kernel will clamp
/// the copy to the smaller of `len` and `dest.len()`.
pub fn read_hibernated_region(
    addr: u32,
    len: u32,
    dest: &mut [u8],
) -> Result<usize, u32> {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&addr.to_le_bytes());
    buf[4..].copy_from_slice(&len.to_le_bytes());

    let mut leases = [Lease::write_only(dest)];
    let (rc, copied) = userlib::sys_send_to_kernel(
        Kipcnum::ReadHibernatedRegion as u16,
        &mut buf,
        8,
        &mut leases,
    );
    if rc == userlib::ResponseCode::SUCCESS {
        Ok(copied)
    } else {
        Err(rc.0)
    }
}

/// Writes `src` into an active hibernated region starting at `addr`,
/// up to `len` bytes. The kernel uses a readable lease at lease index 0
/// to pull bytes out of the supervisor.
///
/// Returns the number of bytes actually copied.
pub fn write_hibernated_region(
    addr: u32,
    len: u32,
    src: &[u8],
) -> Result<usize, u32> {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&addr.to_le_bytes());
    buf[4..].copy_from_slice(&len.to_le_bytes());

    let mut leases = [Lease::read_only(src)];
    let (rc, copied) = userlib::sys_send_to_kernel(
        Kipcnum::WriteHibernatedRegion as u16,
        &mut buf,
        8,
        &mut leases,
    );
    if rc == userlib::ResponseCode::SUCCESS {
        Ok(copied)
    } else {
        Err(rc.0)
    }
}

/// Restores a hibernated region identified by `event_id`. `mode` selects
/// whether tasks blocked on the region resume cleanly or take a
/// `LostRegion` fault.
pub fn restore_region(
    event_id: HibernationEventId,
    mode: RestoreMode,
) -> Result<(), u32> {
    // ssmarshal serializes `HibernationEventId(u32)` as 4 bytes and
    // `RestoreMode` (#[repr(u32)] enum) as 4 bytes.
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&event_id.0.to_le_bytes());
    buf[4..].copy_from_slice(&(mode as u32).to_le_bytes());

    let (rc, _) = userlib::sys_send_to_kernel(
        Kipcnum::RestoreRegion as u16,
        &mut buf,
        8,
        &mut [],
    );
    if rc == userlib::ResponseCode::SUCCESS {
        Ok(())
    } else {
        Err(rc.0)
    }
}

/// Asks the kernel to suspend the task at the given index (e.g. for
/// debugger attach). The kernel rejects suspending the supervisor.
pub fn suspend_task(task_index: usize) {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&(task_index as u32).to_le_bytes());
    let _ = userlib::sys_send_to_kernel(
        Kipcnum::SuspendTask as u16,
        &mut buf,
        4,
        &mut [],
    );
}

/// Clears the debug-suspend flag on the task at the given index. If the
/// task is no longer blocked on any hibernated region, it will resume.
pub fn restore_task(task_index: usize) {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&(task_index as u32).to_le_bytes());
    let _ = userlib::sys_send_to_kernel(
        Kipcnum::RestoreTask as u16,
        &mut buf,
        4,
        &mut [],
    );
}
