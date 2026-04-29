//! API for the kernel's `kipc` interface. This should only be used from
//! supervisor tasks.

#![no_std]

use core::{num::NonZeroUsize, sync::atomic::Ordering};

use abi::Kipcnum;
pub use abi::TaskState;

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
