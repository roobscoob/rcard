//! API for the kernel's `kipc` interface. This should only be used from
//! supervisor tasks.

#![no_std]

use core::{num::NonZeroUsize, sync::atomic::Ordering};

use abi::Kipcnum;
pub use abi::TaskState;

/// Reads the scheduling/fault status of the task with the given index.
pub fn read_task_status(task_index: usize) -> TaskState {
    let mut response = [0; size_of::<TaskState>()];
    let (_, len) = userlib::sys_send_to_kernel(
        Kipcnum::ReadTaskStatus as u16,
        &(task_index as u32).to_le_bytes(),
        &mut response,
        &mut [],
    );
    match ssmarshal::deserialize(&response[..len]) {
        Ok((state, _)) => state,
        Err(_) => panic!(),
    }
}

/// Scans tasks from `task_index` looking for a task that has failed.
pub fn find_faulted_task(task_index: usize) -> Option<NonZeroUsize> {
    let mut response = [0; 4];
    let (_, _len) = userlib::sys_send_to_kernel(
        Kipcnum::FindFaultedTask as u16,
        &(task_index as u32).to_le_bytes(),
        &mut response,
        &mut [],
    );
    let i = u32::from_le_bytes(response);
    NonZeroUsize::new(i as usize)
}

/// Requests that the task at a given index be reinitialized and optionally started.
pub fn reinitialize_task(task_index: usize, new_state: NewState) {
    let mut msg = [0; 5];
    msg[..4].copy_from_slice(&(task_index as u32).to_le_bytes());
    msg[4] = new_state as u8;

    let _ = userlib::sys_send_to_kernel(
        Kipcnum::ReinitTask as u16,
        &msg,
        &mut [],
        &mut [],
    );
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NewState {
    Halted = 0,
    Runnable = 1,
}

pub fn reset() -> ! {
    userlib::sys_send_to_kernel(Kipcnum::Reset as u16, &[], &mut [], &mut []);
    loop {
        core::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}
