//! Kernel abstraction layer.
//!
//! On ARM (Hubris target): thin re-exports from `userlib`.
//! On host (test target): portable type definitions + fake kernel.

// ── ARM target: re-export everything from userlib ────────────────────

#[cfg(target_arch = "arm")]
pub use userlib::{
    // Types
    BorrowInfo, Gen, Lease, LeaseAttributes, Message, MessageOrNotification, ReplyFaultReason,
    ResponseCode, TaskDeath, TaskId, Truncated,
    // Syscalls
    sys_borrow_info, sys_borrow_read, sys_borrow_write, sys_panic, sys_post, sys_recv_msg_open,
    sys_recv_open, sys_refresh_task_id, sys_reply, sys_reply_fault, sys_send,
};

// ── Host target: portable definitions ────────────────────────────────

#[cfg(not(target_arch = "arm"))]
mod host;

#[cfg(not(target_arch = "arm"))]
pub use host::*;
