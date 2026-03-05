use core::arch::global_asm;
use core::mem::MaybeUninit;
use crate::{Lease, AbiLease, TaskId, Sysnum, TaskDeath, Truncated, ResponseCode, Message, MessageOrNotification, TimerSettings, ReplyFaultReason, LeaseAttributes};

extern "Rust" {
    /// Unresolved symbol for the application main function.
    fn main() -> !;
}

extern "C" {
    /// Our actual startup routine.
    fn _start() -> !;
}

pub(crate) fn idle() {
    cortex_m::asm::wfi();
}

#[inline(always)]
pub fn sys_send(
    target: TaskId,
    operation: u16,
    outgoing: &[u8],
    incoming: &mut [u8],
    leases: &mut [Lease<'_>],
) -> Result<(ResponseCode, usize), TaskDeath> {
    let target_and_operation = u32::from(target.0) << 16
            | u32::from(operation);
    let ret64 = unsafe {
        sys_send_stub(
            target_and_operation,
            outgoing.as_ptr(),
            outgoing.len(),
            incoming.as_mut_ptr(),
            incoming.len(),
            leases.as_mut_ptr().cast(),
            leases.len(),
        )
    };
    let retval = ResponseCode(ret64 as u32);
    if let Ok(e) = TaskDeath::try_from(retval) {
        Err(e)
    } else {
        Ok((retval, (ret64 >> 32) as usize))
    }
}

#[inline(always)]
pub fn sys_send_to_kernel(
    operation: u16,
    outgoing: &[u8],
    incoming: &mut [u8],
    leases: &mut [Lease<'_>],
) -> (ResponseCode, usize) {
    let target_and_operation = u32::from(TaskId::KERNEL.0) << 16
            | u32::from(operation);
    let ret64 = unsafe {
        sys_send_stub(
            target_and_operation,
            outgoing.as_ptr(),
            outgoing.len(),
            incoming.as_mut_ptr(),
            incoming.len(),
            leases.as_mut_ptr().cast(),
            leases.len(),
        )
    };
    let retval = ResponseCode(ret64 as u32);
    (retval, (ret64 >> 32) as usize)
}

cfg_if::cfg_if! {
    if #[cfg(any(
        // It'd sure be nice if we could detect proper thumb2
        hubris_target = "thumbv7m-none-eabi",
        hubris_target = "thumbv7em-none-eabi",
        hubris_target = "thumbv7em-none-eabihf",
        hubris_target = "thumbv8m.main-none-eabi",
        hubris_target = "thumbv8m.main-none-eabihf",
    ))] {
        global_asm!("
        .section .text.sys_send_stub
        .globl sys_send_stub
        .type sys_send_stub,function
        sys_send_stub:
            .cfi_startproc

            @ We get the first four arguments in r0-r3, and the next three from the stack.
            @ We need one additional working register for the syscall number, for a total
            @ of eight.

            @ Stash the register values we're about to destroy.
            push {{r4-r11, lr}}
            .cfi_adjust_cfa_offset 36
            .cfi_offset r4, -36
            .cfi_offset r5, -32
            .cfi_offset r6, -28
            .cfi_offset r7, -24
            .cfi_offset r8, -20
            .cfi_offset r9, -16
            .cfi_offset r10, -12
            .cfi_offset r11, -8
            .cfi_offset lr, -4

            @ Materialize the sysnum constant.
            mov r11, #{sysnum}

            @ Load the three operands from the stack. We've pushed nine words onto
            @ the stack, so we need to address _past_ that.
            add r4, sp, #(9 * 4)
            ldm r4, {{r8-r10}}

            @ Copy up register operands.
            mov r4, r0
            mov r5, r1
            mov r6, r2
            mov r7, r3

            svc #0

            @ Put the results into the function return position.
            mov r0, r4
            mov r1, r5
            @ Restore the registers.
            pop {{r4-r11, pc}}
            .cfi_endproc
        ",
            sysnum = const Sysnum::Send as u32,
        );
    } else {
        // Fall back to the generally applicable, ARMv6-M-compatible subset.
        global_asm!("
        .section .text.sys_send_stub
        .globl sys_send_stub
        .type sys_send_stub,function
        sys_send_stub:
            .cfi_startproc

            @ We get the first four arguments in r0-r3, and the next three on the stack.
            @ We need one additional working register for the syscall number, for a total
            @ of eight.

            @ Stash the register values we're about to destroy.
            push {{r4-r7, lr}}
            .cfi_adjust_cfa_offset 20
            .cfi_offset r4, -20
            .cfi_offset r5, -16
            .cfi_offset r6, -12
            .cfi_offset r7, -8
            .cfi_offset lr, -4

            mov r4, r8
            mov r5, r9
            mov r6, r10
            mov r7, r11
            push {{r4-r7}}
            .cfi_adjust_cfa_offset 16
            .cfi_offset r4, -36
            .cfi_offset r5, -32
            .cfi_offset r6, -28
            .cfi_offset r7, -24

            @ Materialize the sysnum constant. For Thumb Reasons this has to go through
            @ r4 on the way, so we do this while we still have a bunch of temp registers
            @ free.
            movs r4, #{sysnum}
            mov r11, r4

            @ Load the three operands from the stack. We've pushed nine words onto
            @ the stack, so we need to address _past_ that.
            add r4, sp, #(9 * 4)
            ldm r4, {{r4-r6}}
            mov r8, r4
            mov r9, r5
            mov r10, r6

            @ Copy up register operands.
            mov r4, r0
            mov r5, r1
            mov r6, r2
            mov r7, r3

            svc #0

            @ Put the results into the function return position.
            mov r0, r4
            mov r1, r5
            @ Restore the registers.
            pop {{r4-r7}}
            .cfi_adjust_cfa_offset -16
            mov r8, r4
            mov r9, r5
            mov r10, r6
            mov r11, r7
            pop {{r4-r7, pc}}

            .cfi_endproc
        ",
            sysnum = const Sysnum::Send as u32,
        );
    }
}
/// The actual return register layout after a call to `recv`.
///
/// This lets us blit the return registers directly into this struct from the
/// assembly stub without having to think too much.
#[repr(C)]
struct AbiRecvMessage {
    sender: u32,
    operation_or_notification: u32,
    sent_length: usize,
    reply_capacity: usize,
    lease_count: usize,
}

#[inline(always)]
pub fn sys_recv(
    incoming: &mut [MaybeUninit<u8>],
    notification_mask: u32,
    from: Option<TaskId>,
) -> Result<MessageOrNotification<'_>, TaskDeath> {
    let mut out = MaybeUninit::<AbiRecvMessage>::uninit();
    let retval = unsafe {
        sys_recv_stub(
            incoming.as_mut_ptr().cast(),
            incoming.len(),
            notification_mask,
            from.map(|tid| 0x8000_0000 | u32::from(tid.0)).unwrap_or(0),
            out.as_mut_ptr(),
        )
    };

    // Safety: our asm stub is responsible for fully initializing this struct
    // whether we succeed or fail.
    let retval = ResponseCode(retval);
    if let Ok(e) = TaskDeath::try_from(retval) {
        return Err(e);
    }
    let rm = unsafe { out.assume_init() };
    if rm.sender as u16 == u16::from(TaskId::KERNEL) {
        Ok(MessageOrNotification::Notification(rm.operation_or_notification))
    } else {
        let data = if rm.sent_length <= incoming.len() {
            unsafe {
                Ok(core::slice::from_raw_parts(
                        incoming.as_ptr().cast(),
                        rm.sent_length,
                ))
            }
        } else {
            Err(Truncated)
        };
        Ok(MessageOrNotification::Message(Message {
            sender: TaskId(rm.sender as u16),
            operation: rm.operation_or_notification as u16,
            data,
            reply_capacity: rm.reply_capacity,
            lease_count: rm.lease_count,
        }))
    }
}

#[inline(always)]
pub fn sys_recv_msg(
    incoming: &mut [MaybeUninit<u8>],
    from: Option<TaskId>,
) -> Result<Message<'_>, TaskDeath> {
    let mut out = MaybeUninit::<AbiRecvMessage>::uninit();
    let retval = unsafe {
        sys_recv_stub(
            incoming.as_mut_ptr().cast(),
            incoming.len(),
            0,
            from.map(|tid| 0x8000_0000 | u32::from(tid.0)).unwrap_or(0),
            out.as_mut_ptr(),
        )
    };

    // Safety: our asm stub is responsible for fully initializing this struct
    // whether we succeed or fail.
    let retval = ResponseCode(retval);
    if let Ok(e) = TaskDeath::try_from(retval) {
        return Err(e);
    }
    let rm = unsafe { out.assume_init() };
    // We don't need to check the sender, we set the notification mask to 0.
    let data = if rm.sent_length <= incoming.len() {
        unsafe {
            Ok(core::slice::from_raw_parts(
                    incoming.as_ptr().cast(),
                    rm.sent_length,
            ))
        }
    } else {
        Err(Truncated)
    };
    Ok(Message {
        sender: TaskId(rm.sender as u16),
        operation: rm.operation_or_notification as u16,
        data,
        reply_capacity: rm.reply_capacity,
        lease_count: rm.lease_count,
    })
}

#[inline(always)]
pub fn sys_recv_open(
    incoming: &mut [MaybeUninit<u8>],
    notification_mask: u32,
) -> MessageOrNotification<'_> {
    let mut out = MaybeUninit::<AbiRecvMessage>::uninit();
    unsafe {
        sys_recv_stub(
            incoming.as_mut_ptr().cast(),
            incoming.len(),
            notification_mask,
            0,
            out.as_mut_ptr(),
        );
    }

    // Safety: our asm stub is responsible for fully initializing this struct
    // whether we succeed or fail.
    let rm = unsafe { out.assume_init() };
    if rm.sender as u16 == u16::from(TaskId::KERNEL) {
        MessageOrNotification::Notification(rm.operation_or_notification)
    } else {
        let data = if rm.sent_length <= incoming.len() {
            unsafe {
                Ok(core::slice::from_raw_parts(
                        incoming.as_ptr().cast(),
                        rm.sent_length,
                ))
            }
        } else {
            Err(Truncated)
        };
        MessageOrNotification::Message(Message {
            sender: TaskId(rm.sender as u16),
            operation: rm.operation_or_notification as u16,
            data,
            reply_capacity: rm.reply_capacity,
            lease_count: rm.lease_count,
        })
    }
}

#[inline(always)]
pub fn sys_recv_msg_open(
    incoming: &mut [MaybeUninit<u8>],
) -> Message<'_> {
    let mut out = MaybeUninit::<AbiRecvMessage>::uninit();
    unsafe {
        sys_recv_stub(
            incoming.as_mut_ptr().cast(),
            incoming.len(),
            0, // notification mask
            0, // specific sender
            out.as_mut_ptr(),
        );
    }

    // Safety: our asm stub is responsible for fully initializing this struct
    // whether we succeed or fail.
    let rm = unsafe { out.assume_init() };
    // We don't need to check the sender, we set notification mask to 0.
    let data = if rm.sent_length <= incoming.len() {
        unsafe {
            Ok(core::slice::from_raw_parts(
                    incoming.as_ptr().cast(),
                    rm.sent_length,
            ))
        }
    } else {
        Err(Truncated)
    };
    Message {
        sender: TaskId(rm.sender as u16),
        operation: rm.operation_or_notification as u16,
        data,
        reply_capacity: rm.reply_capacity,
        lease_count: rm.lease_count,
    }
}

#[inline(always)]
pub fn sys_recv_notification(
    notification_mask: u32,
) -> u32 {
    let mut out = MaybeUninit::<AbiRecvMessage>::uninit();
    let retval = unsafe {
        sys_recv_stub(
            core::ptr::null_mut(),
            0,
            notification_mask,
            0x8000_0000 | u32::from(TaskId::KERNEL.0),
            out.as_mut_ptr(),
        )
    };

    // This can't actually fail.
    let _ = retval;

    let rm = unsafe { out.assume_init() };
    rm.operation_or_notification
}

cfg_if::cfg_if! {
    if #[cfg(any(
        // It'd sure be nice if we could detect proper thumb2
        hubris_target = "thumbv7m-none-eabi",
        hubris_target = "thumbv7em-none-eabi",
        hubris_target = "thumbv7em-none-eabihf",
        hubris_target = "thumbv8m.main-none-eabi",
        hubris_target = "thumbv8m.main-none-eabihf",
    ))] {
        global_asm!("
        .section .text.sys_recv_stub
        .globl sys_recv_stub
        .type sys_recv_stub,function
        sys_recv_stub:
            .cfi_startproc

            @ Stash the register values we're about to destroy.
            push {{r4-r11, lr}}
            .cfi_adjust_cfa_offset 36
            .cfi_offset r4, -36
            .cfi_offset r5, -32
            .cfi_offset r6, -28
            .cfi_offset r7, -24
            .cfi_offset r8, -20
            .cfi_offset r9, -16
            .cfi_offset r10, -12
            .cfi_offset r11, -8
            .cfi_offset lr, -4

            @ Materialize the sysnum constant.
            mov r11, #{sysnum}

            @ Move arguments 0-3 into the correct registers.
            mov r4, r0
            mov r5, r1
            mov r6, r2
            mov r7, r3

            @ Collect the final argument from the stack. This isn't an argument to the
            @ kernel, it's our out-buffer-pointer. We could technically do this _after_
            @ the syscall.
            ldr r3, [sp, #(9 * 4)]

            svc #0

            @ Status flag into return position.
            mov r0, r4
            @ Write the rest through our out-buffer pointer.
            stm r3!, {{r5-r9}}

            @ Restore the registers.
            pop {{r4-r11, pc}}
            .cfi_endproc
        ",
            sysnum = const Sysnum::Recv as u32,
        );
    } else {
        global_asm!("
        .section .text.sys_recv_stub
        .globl sys_recv_stub
        .type sys_recv_stub,function
        sys_recv_stub:
            .cfi_startproc

            @ Stash the register values we're about to destroy.
            push {{r4-r7, lr}}
            .cfi_adjust_cfa_offset 20
            .cfi_offset r4, -20
            .cfi_offset r5, -16
            .cfi_offset r6, -12
            .cfi_offset r7, -8
            .cfi_offset lr, -4

            mov r4, r8
            mov r5, r9
            mov r6, r10
            mov r7, r11
            push {{r4-r7}}
            .cfi_adjust_cfa_offset 16
            .cfi_offset r4, -36
            .cfi_offset r5, -32
            .cfi_offset r6, -28
            .cfi_offset r7, -24

            @ Materialize the sysnum constant.
            eors r4, r4
            adds r4, #{sysnum}
            mov r11, r4

            @ Move arguments 0-3 into the correct registers.
            mov r4, r0
            mov r5, r1
            mov r6, r2
            mov r7, r3

            @ Collect the final argument from the stack. This isn't an argument to the
            @ kernel, it's our out-buffer-pointer. We could technically do this _after_
            @ the syscall.
            ldr r3, [sp, #(9 * 4)]

            svc #0

            @ Status flag into return position.
            mov r0, r4
            @ Write the rest through our out-buffer pointer.
            stm r3!, {{r5-r7}}
            mov r5, r8
            mov r6, r9
            stm r3!, {{r5-r6}}

            @ Restore the registers.
            pop {{r4-r7}}
            .cfi_adjust_cfa_offset -16
            mov r8, r4
            mov r9, r5
            mov r10, r6
            mov r11, r7
            pop {{r4-r7, pc}}

            .cfi_endproc
        ",
            sysnum = const Sysnum::Recv as u32,
        );
    }
}

#[inline(always)]
pub fn sys_reply(
    sender: TaskId,
    code: ResponseCode,
    message: &[u8],
) {
    unsafe {
        sys_reply_stub(
            sender.0 as u32,
            code.0,
            message.as_ptr(),
            message.len(),
        )
    }
}

cfg_if::cfg_if! {
    if #[cfg(any(
        // It'd sure be nice if we could detect proper thumb2
        hubris_target = "thumbv7m-none-eabi",
        hubris_target = "thumbv7em-none-eabi",
        hubris_target = "thumbv7em-none-eabihf",
        hubris_target = "thumbv8m.main-none-eabi",
        hubris_target = "thumbv8m.main-none-eabihf",
    ))] {
        global_asm!("
        .section .text.sys_reply_stub
        .globl sys_reply_stub
        .type sys_reply_stub,function
        sys_reply_stub:
            .cfi_startproc

            @ Stash the register values we're about to destroy.
            push {{r4-r7, r11, lr}}
            .cfi_adjust_cfa_offset 24
            .cfi_offset r4, -24
            .cfi_offset r5, -20
            .cfi_offset r6, -16
            .cfi_offset r7, -12
            .cfi_offset r11, -8
            .cfi_offset lr, -4


            @ Materialize the sysnum constant.
            mov r11, #{sysnum}

            @ Move arguments 0-3 into the correct registers.
            mov r4, r0
            mov r5, r1
            mov r6, r2
            mov r7, r3

            svc #0

            @ Restore the registers.
            pop {{r4-r7, r11, pc}}

            .cfi_endproc
        ",
            sysnum = const Sysnum::Reply as u32,
        );
    } else {
        global_asm!("
        .section .text.sys_reply_stub
        .globl sys_reply_stub
        .type sys_reply_stub,function
        sys_reply_stub:
            .cfi_startproc

            @ Stash the register values we're about to destroy.
            push {{r4-r7, lr}}
            .cfi_adjust_cfa_offset 20
            .cfi_offset r4, -20
            .cfi_offset r5, -16
            .cfi_offset r6, -12
            .cfi_offset r7, -8
            .cfi_offset lr, -4

            mov r4, r11
            push {{r4}}
            .cfi_adjust_cfa_offset 4
            .cfi_offset r11, -24

            @ Materialize the sysnum constant.
            eors r4, r4
            adds r4, #{sysnum}
            mov r11, r4

            @ Move arguments 0-3 into the correct registers.
            mov r4, r0
            mov r5, r1
            mov r6, r2
            mov r7, r3

            svc #0

            @ Restore the registers.
            pop {{r4}}
            .cfi_adjust_cfa_offset -4
            mov r11, r4
            pop {{r4-r7, pc}}

            .cfi_endproc
        ",
            sysnum = const Sysnum::Reply as u32,
        );
    }
}

#[inline(always)]
pub fn sys_reply_fault(
    sender: TaskId,
    reason: ReplyFaultReason,
) {
    unsafe {
        sys_reply_fault_stub(
            sender.0 as u32,
            reason as u32,
        )
    }
}

cfg_if::cfg_if! {
    if #[cfg(any(
        // It'd sure be nice if we could detect proper thumb2
        hubris_target = "thumbv7m-none-eabi",
        hubris_target = "thumbv7em-none-eabi",
        hubris_target = "thumbv7em-none-eabihf",
        hubris_target = "thumbv8m.main-none-eabi",
        hubris_target = "thumbv8m.main-none-eabihf",
    ))] {
        global_asm!("
        .section .text.sys_reply_fault_stub
        .globl sys_reply_fault_stub
        .type sys_reply_fault_stub,function
        sys_reply_fault_stub:
            .cfi_startproc

            @ Stash the register values we're about to destroy.
            push {{r4-r5, r11}}
            .cfi_adjust_cfa_offset 12
            .cfi_offset r4, -12
            .cfi_offset r5, -8
            .cfi_offset, r11, -4

            @ Materialize the sysnum constant.
            mov r11, #{sysnum}

            @ Move arguments 0-1 into the correct registers.
            mov r4, r0
            mov r5, r1

            svc #0

            @ Restore the registers.
            pop {{r4-r5, r11}}
            .cfi_adjust_cfa_offset -12
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::ReplyFault as u32,
        );
    } else {
        global_asm!("
        .section .text.sys_reply_fault_stub
        .globl sys_reply_fault_stub
        .type sys_reply_fault_stub,function
        sys_reply_fault_stub:
            .cfi_startproc

            @ Stash the register values we're about to destroy.
            push {{r4-r5}}
            .cfi_adjust_cfa_offset 8
            .cfi_offset r4, -8
            .cfi_offset r5, -4

            mov r4, r11
            push {{r4}}
            .cfi_adjust_cfa_offset 4
            .cfi_offset r11, -12

            @ Materialize the sysnum constant.
            eors r4, r4
            adds r4, #{sysnum}
            mov r11, r4

            @ Move arguments 0-1 into the correct registers.
            mov r4, r0
            mov r5, r1

            svc #0

            @ Restore the registers.
            pop {{r4}}
            .cfi_adjust_cfa_offset -4
            mov r11, r4
            pop {{r4-r5}}
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::ReplyFault as u32,
        );
    }
}

pub fn sys_panic(msg: &[u8]) -> ! {
    unsafe {
        sys_panic_stub(msg.as_ptr(), msg.len())
    }
}

cfg_if::cfg_if! {
    if #[cfg(any(
        // It'd sure be nice if we could detect proper thumb2
        hubris_target = "thumbv7m-none-eabi",
        hubris_target = "thumbv7em-none-eabi",
        hubris_target = "thumbv7em-none-eabihf",
        hubris_target = "thumbv8m.main-none-eabi",
        hubris_target = "thumbv8m.main-none-eabihf",
    ))] {
        global_asm!("
        .section .text.sys_panic_stub
        .globl sys_panic_stub
        .type sys_panic_stub,function
        sys_panic_stub:
            .cfi_startproc

            @ Stash the register values we're about to destroy, to make reconstructing the
            @ task's state at panic easier. (Since the task won't resume, these will never
            @ get popped.)
            push {{r4, r5, r11, lr}}
            .cfi_adjust_cfa_offset 16
            .cfi_offset r4, -16
            .cfi_offset r5, -12
            .cfi_offset r11, -8
            .cfi_offset lr, -4

            @ Move arguments into place.
            mov r4, r0
            mov r5, r1
            mov r11, {sysnum}

            svc #0
            udf 0xad
            .cfi_endproc
        ",
            sysnum = const Sysnum::Panic as u32,
        );
    } else if #[cfg(hubris_target = "thumbv6m-none-eabi")] {
        global_asm!("
        .section .text.sys_panic_stub
        .globl sys_panic_stub
        .type sys_panic_stub,function
        sys_panic_stub:
            .cfi_startproc

            @ Stash the register values we're about to destroy, to make reconstructing the
            @ task's state at panic easier. (Since the task won't resume, these will never
            @ get popped.)
            push {{r4, r5, lr}}
            .cfi_adjust_cfa_offset 16
            .cfi_offset r4, -16
            .cfi_offset r5, -12
            .cfi_offset r11, -8
            .cfi_offset lr, -4
            mov r4, r11
            .cfi_register r11, r4
            push {{r4}}
            .cfi_offset r11, -20
            .cfi_adjust_cfa_offset 4

            @ Materialize the sysnum constant.
            eors r4, r4
            adds r4, #{sysnum}
            mov r11, r4
            @ Move arguments into place.
            mov r4, r0
            mov r5, r1

            svc #0
            udf 0xad
            .cfi_endproc
        ",
            sysnum = const Sysnum::Panic as u32,
        );
    } else {
        compile_error!("unrecognized ARM target triple");
    }
}

cfg_if::cfg_if! {
    if #[cfg(any(
        // It'd sure be nice if we could detect proper thumb2
        hubris_target = "thumbv7m-none-eabi",
        hubris_target = "thumbv7em-none-eabi",
        hubris_target = "thumbv7em-none-eabihf",
        hubris_target = "thumbv8m.main-none-eabi",
        hubris_target = "thumbv8m.main-none-eabihf",
    ))] {
        global_asm!("
        .section .text.sys_set_timer_stub
        .globl sys_set_timer_stub
        .type sys_set_timer_stub,function
        sys_set_timer_stub:
            .cfi_startproc

            @ Stash the register values we're about to destroy.
            push {{r4-r7, r11}}
            .cfi_adjust_cfa_offset 20
            .cfi_offset r4, -20
            .cfi_offset r5, -16
            .cfi_offset r6, -12
            .cfi_offset r7, -8
            .cfi_offset r11, -4

            @ Materialize the sysnum constant.
            mov r11, #{sysnum}

            @ Move arguments 0-3 into the correct registers.
            mov r4, r0
            mov r5, r1
            mov r6, r2
            mov r7, r3

            svc #0

            @ Restore the registers.
            pop {{r4-r7, r11}}
            .cfi_adjust_cfa_offset -20
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::SetTimer as u32,
        );
    } else {
        global_asm!("
        .section .text.sys_set_timer_stub
        .globl sys_set_timer_stub
        .type sys_set_timer_stub,function
        sys_set_timer_stub:
            .cfi_startproc

            @ Stash the register values we're about to destroy.
            push {{r4-r7}}
            .cfi_adjust_cfa_offset 16
            .cfi_offset r4, -16
            .cfi_offset r5, -12
            .cfi_offset r6, -8
            .cfi_offset r7, -4

            mov r4, r11
            push {{r4}}
            .cfi_adjust_cfa_offset 4
            .cfi_offset r11, -20

            @ Materialize the sysnum constant.
            eors r4, r4
            adds r4, #{sysnum}
            mov r11, r4

            @ Move arguments 0-3 into the correct registers.
            mov r4, r0
            mov r5, r1
            mov r6, r2
            mov r7, r3

            svc #0

            @ Restore the registers.
            pop {{r4}}
            .cfi_adjust_cfa_offset -4
            mov r11, r4
            pop {{r4-r7}}
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::SetTimer as u32,
        );
    }
}
pub fn sys_set_timer(
    deadline: Option<u64>,
    notifications: u32,
) {
    let raw_deadline = deadline.unwrap_or(0);
    unsafe {
        sys_set_timer_stub(
            deadline.is_some() as u32,
            raw_deadline as u32,
            (raw_deadline >> 32) as u32,
            notifications,
        )
    }
}

/// A version of `TimerSettings` with its fields in exactly the order of the
/// return registers after a `get-timer` call.
///
/// This ensures that we can blit the return registers into this directly from
/// the asm stub, without having to think too much.
#[repr(C)]
struct AbiTimerSettings {
    now: [u32; 2],
    deadline_set: u32,
    deadline: [u32; 2],
    notification: u32,
}

#[inline(always)]
pub fn sys_get_timer() -> TimerSettings {
    let out = unsafe {
        sys_get_timer_stub()
    };

    TimerSettings {
        now: u64::from(out.now[0]) | u64::from(out.now[1]) << 32,
        alarm: if out.deadline_set != 0 {
            Some((
                u64::from(out.deadline[0]) | u64::from(out.deadline[1]) << 32,
                out.notification,
            ))
        } else {
            None
        },
    }
}


cfg_if::cfg_if! {
    if #[cfg(any(
        // It'd sure be nice if we could detect proper thumb2
        hubris_target = "thumbv7m-none-eabi",
        hubris_target = "thumbv7em-none-eabi",
        hubris_target = "thumbv7em-none-eabihf",
        hubris_target = "thumbv8m.main-none-eabi",
        hubris_target = "thumbv8m.main-none-eabihf",
    ))] {
        global_asm!("
        .section .text.sys_get_timer_stub
        .globl sys_get_timer_stub
        .type sys_get_timer_stub,function
        sys_get_timer_stub:
            .cfi_startproc

            @ Stash the register values that will be destroyed by the returned data.
            push {{r4-r9, r11}}
            .cfi_adjust_cfa_offset 28
            .cfi_offset r4, -28
            .cfi_offset r5, -24
            .cfi_offset r6, -20
            .cfi_offset r7, -16
            .cfi_offset r8, -12
            .cfi_offset r9, -8
            .cfi_offset r11, -4

            @ Materialize the sysnum constant.
            mov r11, #{sysnum}

            svc #0

            @ Copy outputs into the return struct.
            stm r0!, {{r4-r9}}

            @ Restore the registers.
            pop {{r4-r9, r11}}
            .cfi_adjust_cfa_offset -28
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::GetTimer as u32,
        );
    } else {
        global_asm!("
        .section .text.sys_get_timer_stub
        .globl sys_get_timer_stub
        .type sys_get_timer_stub,function
        sys_get_timer_stub:
            .cfi_startproc

            @ Stash the register values that will be destroyed by the returned data.
            push {{r4-r7}}
            .cfi_adjust_cfa_offset 16
            .cfi_offset r4, -16
            .cfi_offset r5, -12
            .cfi_offset r6, -8
            .cfi_offset r7, -4

            mov r4, r8
            mov r5, r9
            mov r6, r11
            push {{r4-r6}}
            .cfi_adjust_cfa_offset 12
            .cfi_offset r8, -28
            .cfi_offset r9, -24
            .cfi_offset r11, -20

            @ Materialize the sysnum constant.
            eors r4, r4
            adds r4, #{sysnum}
            mov r11, r4

            svc #0

            @ Copy outputs into the return struct.
            stm r0!, {{r4-r7}}
            mov r4, r8
            mov r5, r9
            stm r0!, {{r4-r5}}

            @ Restore the registers.
            pop {{r4-r6}}
            .cfi_adjust_cfa_offset -12
            mov r8, r4
            mov r9, r5
            mov r11, r6
            pop {{r4-r7}}
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::GetTimer as u32,
        );
    }
}
#[inline(always)]
pub fn sys_enable_irq(bits: u32) {
    unsafe {
        sys_irq_control_stub(bits, 1)
    }
}

#[inline(always)]
pub fn sys_enable_irq_and_clear_pending(bits: u32) {
    unsafe {
        sys_irq_control_stub(bits, 0b11)
    }
}

cfg_if::cfg_if! {
    if #[cfg(any(
        // It'd sure be nice if we could detect proper thumb2
        hubris_target = "thumbv7m-none-eabi",
        hubris_target = "thumbv7em-none-eabi",
        hubris_target = "thumbv7em-none-eabihf",
        hubris_target = "thumbv8m.main-none-eabi",
        hubris_target = "thumbv8m.main-none-eabihf",
    ))] {
        global_asm!("
        .section .text.sys_irq_control_stub
        .globl sys_irq_control_stub
        .type sys_irq_control_stub,function
        sys_irq_control_stub:
            .cfi_startproc

            @ Stash the register values that we'll use to carry arguments.
            push {{r4-r5, r11}}
            .cfi_adjust_cfa_offset 12
            .cfi_offset r4, -12
            .cfi_offset r5, -8
            .cfi_offset r11, -4

            @ Materialize the sysnum constant.
            mov r11, #{sysnum}

            @ Move arguments into place.
            mov r4, r0
            mov r5, r1

            svc #0

            @ Restore the registers.
            pop {{r4-r5, r11}}
            .cfi_adjust_cfa_offset -12
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::IrqControl as u32,
        );
    } else {
        global_asm!("
        .section .text.sys_irq_control_stub
        .globl sys_irq_control_stub
        .type sys_irq_control_stub,function
        sys_irq_control_stub:
            .cfi_startproc

            @ Stash the register values that we'll use to carry arguments.
            push {{r4-r5}}
            .cfi_adjust_cfa_offset 8
            .cfi_offset r4, -8
            .cfi_offset r5, -4

            mov r4, r11
            push {{r4}}
            .cfi_adjust_cfa_offset 4
            .cfi_offset r11, -12

            @ Materialize the sysnum constant.
            eors r4, r4
            adds r4, #{sysnum}
            mov r11, r4

            @ Move arguments into place.
            mov r4, r0
            mov r5, r1

            svc #0

            @ Restore the registers.
            pop {{r4}}
            .cfi_adjust_cfa_offset -4
            mov r11, r4
            pop {{r4-r5}}
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::IrqControl as u32,
        );
    }
}

#[repr(C)]
struct AbiBorrowInfo {
    rc: u32,
    atts: u32,
    len: u32,
}

#[inline(always)]
pub fn sys_borrow_read(
    lender: TaskId,
    index: usize,
    offset: usize,
    dest: &mut [u8],
) -> Option<usize> {
    let info64 = unsafe {
        sys_borrow_read_stub(
            lender.0 as u32,
            index as u32,
            offset as u32,
            dest.as_mut_ptr(),
            dest.len(),
        )
    };
    if info64 as u32 == 0 {
        Some((info64 >> 32) as usize)
    } else {
        None
    }
}

#[inline(always)]
pub fn sys_borrow_write(
    lender: TaskId,
    index: usize,
    offset: usize,
    src: &[u8],
) -> Option<usize> {
    let info64 = unsafe {
        sys_borrow_write_stub(
            lender.0 as u32,
            index as u32,
            offset as u32,
            src.as_ptr(),
            src.len(),
        )
    };
    if info64 as u32 == 0 {
        Some((info64 >> 32) as usize)
    } else {
        None
    }
}

#[inline(always)]
pub fn sys_borrow_info(
    lender: TaskId,
    index: usize,
) -> Option<crate::BorrowInfo> {
    let mut info = MaybeUninit::uninit();
    unsafe {
        sys_borrow_info_stub(
            lender.0 as u32,
            index as u32,
            info.as_mut_ptr(),
        )
    }
    let info = unsafe { info.assume_init() };

    if info.rc == 0 {
        Some(crate::BorrowInfo {
            atts: LeaseAttributes::from_bits_truncate(info.atts),
            len: info.len as usize,
        })
    } else {
        None
    }
}


cfg_if::cfg_if! {
    if #[cfg(any(
        // It'd sure be nice if we could detect proper thumb2
        hubris_target = "thumbv7m-none-eabi",
        hubris_target = "thumbv7em-none-eabi",
        hubris_target = "thumbv7em-none-eabihf",
        hubris_target = "thumbv8m.main-none-eabi",
        hubris_target = "thumbv8m.main-none-eabihf",
    ))] {
        global_asm!("
        .section .text.sys_borrow_read_stub
        .globl sys_borrow_read_stub
        .type sys_borrow_read_stub,function
        sys_borrow_read_stub:
            .cfi_startproc

            @ Stash the register values that we'll use to carry arguments.
            push {{r4-r8, r11}}
            .cfi_adjust_cfa_offset 24
            .cfi_offset r4, -24
            .cfi_offset r5, -20
            .cfi_offset r6, -16
            .cfi_offset r7, -12
            .cfi_offset r8, -8
            .cfi_offset r11, -4

            @ Materialize the sysnum constant.
            mov r11, #{sysnum}

            @ Move arguments into place.
            mov r4, r0
            mov r5, r1
            mov r6, r2
            mov r7, r3
            ldr r8, [sp, #(6 * 4)]

            svc #0

            @ Copy outputs into place.
            mov r0, r4
            mov r1, r5

            @ Restore the registers.
            pop {{r4-r8, r11}}
            .cfi_adjust_cfa_offset -24
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::BorrowRead as u32,
        );
    } else {
        global_asm!("
        .section .text.sys_borrow_read_stub
        .globl sys_borrow_read_stub
        .type sys_borrow_read_stub,function
        sys_borrow_read_stub:
            .cfi_startproc

            @ Stash the register values that we'll use to carry arguments.
            push {{r4-r7}}
            .cfi_adjust_cfa_offset 16
            .cfi_offset r4, -16
            .cfi_offset r5, -12
            .cfi_offset r6, -8
            .cfi_offset r7, -4

            mov r4, r8
            mov r5, r11
            push {{r4-r5}}
            .cfi_adjust_cfa_offset 8
            .cfi_offset r8, -24
            .cfi_offset r11, -20

            @ Materialize the sysnum constant.
            eors r4, r4
            adds r4, #{sysnum}
            mov r11, r4

            @ Move arguments into place.
            ldr r4, [sp, #(6 * 4)]
            mov r8, r4

            mov r4, r0
            mov r5, r1
            mov r6, r2
            mov r7, r3

            svc #0

            @ Copy outputs into place.
            mov r0, r4
            mov r1, r5

            @ Restore the registers.
            pop {{r4-r5}}
            .cfi_adjust_cfa_offset -8
            mov r8, r4
            mov r11, r5
            pop {{r4-r7}}
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::BorrowRead as u32,
        );
    }
}

global_asm!("
.section .text.sys_borrow_write_stub
.globl sys_borrow_write_stub
.type sys_borrow_write_stub,function
sys_borrow_write_stub:
    .cfi_startproc

    @ Stash the register values that we'll use to carry arguments.
    push {{r4-r7}}
    .cfi_adjust_cfa_offset 16
    .cfi_offset r4, -16
    .cfi_offset r5, -12
    .cfi_offset r6, -8
    .cfi_offset r7, -4

    mov r4, r8
    mov r5, r11
    push {{r4-r5}}
    .cfi_adjust_cfa_offset 8
    .cfi_offset r8, -24
    .cfi_offset r11, -20

    @ Materialize the sysnum constant.
    eors r4, r4
    adds r4, #{sysnum}
    mov r11, r4

    @ Move arguments into place.
    ldr r4, [sp, #(6 * 4)]
    mov r8, r4

    mov r4, r0
    mov r5, r1
    mov r6, r2
    mov r7, r3

    svc #0

    @ Copy outputs into place.
    mov r0, r4
    mov r1, r5

    @ Restore the registers.
    pop {{r4-r5}}
    .cfi_adjust_cfa_offset -8
    mov r8, r4
    mov r11, r5
    pop {{r4-r7}}
    bx lr

    .cfi_endproc
",
    sysnum = const Sysnum::BorrowWrite as u32,
);

cfg_if::cfg_if! {
    if #[cfg(any(
        // It'd sure be nice if we could detect proper thumb2
        hubris_target = "thumbv7m-none-eabi",
        hubris_target = "thumbv7em-none-eabi",
        hubris_target = "thumbv7em-none-eabihf",
        hubris_target = "thumbv8m.main-none-eabi",
        hubris_target = "thumbv8m.main-none-eabihf",
    ))] {
        global_asm!("
        .section .text.sys_borrow_info_stub
        .globl sys_borrow_info_stub
        .type sys_borrow_info_stub,function
        sys_borrow_info_stub:
            .cfi_startproc

            @ Stash the register values that we'll use to carry arguments and receive
            @ results.
            push {{r4-r6, r11}}
            .cfi_adjust_cfa_offset 16
            .cfi_offset r4, -16
            .cfi_offset r5, -12
            .cfi_offset r6, -8
            .cfi_offset r11, -4

            @ Materialize the sysnum constant.
            mov r11, #{sysnum}

            @ Move arguments into place.
            mov r4, r0
            mov r5, r1

            svc #0

            @ Copy outputs into place.
            stm r2!, {{r4, r5, r6}}

            @ Restore the registers.
            pop {{r4-r6, r11}}
            .cfi_adjust_cfa_offset -16
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::BorrowInfo as u32,
        );
    } else {
        global_asm!("
        .section .text.sys_borrow_info_stub
        .globl sys_borrow_info_stub
        .type sys_borrow_info_stub,function
        sys_borrow_info_stub:
            .cfi_startproc

            @ Stash the register values that we'll use to carry arguments and receive
            @ results.
            push {{r4-r6}}
            .cfi_adjust_cfa_offset 12
            .cfi_offset r4, -12
            .cfi_offset r5, -8
            .cfi_offset r6, -4

            mov r4, r11
            push {{r4}}
            .cfi_adjust_cfa_offset 4
            .cfi_offset r11, -16

            @ Materialize the sysnum constant.
            eors r4, r4
            adds r4, #{sysnum}
            mov r11, r4

            @ Move arguments into place.
            mov r4, r0
            mov r5, r1

            svc #0

            @ Copy outputs into place.
            stm r2!, {{r4, r5, r6}}

            @ Restore the registers.
            pop {{r4}}
            .cfi_adjust_cfa_offset -4
            mov r11, r4
            pop {{r4-r6}}
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::BorrowInfo as u32,
        );
    }
}

pub fn sys_post(task: TaskId, notifications: u32) -> Result<(), TaskDeath> {
    let rc = unsafe {
        sys_post_stub(
            task.0 as u32,
            notifications,
        )
    };
    let retval = ResponseCode(rc);
    if let Ok(e) = TaskDeath::try_from(retval) {
        Err(e)
    } else {
        Ok(())
    }
}

cfg_if::cfg_if! {
    if #[cfg(any(
        // It'd sure be nice if we could detect proper thumb2
        hubris_target = "thumbv7m-none-eabi",
        hubris_target = "thumbv7em-none-eabi",
        hubris_target = "thumbv7em-none-eabihf",
        hubris_target = "thumbv8m.main-none-eabi",
        hubris_target = "thumbv8m.main-none-eabihf",
    ))] {
        global_asm!("
        .section .text.sys_post_stub
        .globl sys_post_stub
        .type sys_post_stub,function
        sys_post_stub:
            .cfi_startproc

            @ Stash the register values that we'll use to carry arguments.
            push {{r4-r5, r11}}
            .cfi_adjust_cfa_offset 12
            .cfi_offset r4, -12
            .cfi_offset r5, -8
            .cfi_offset r11, -4

            @ Materialize the sysnum constant.
            mov r11, #{sysnum}

            @ Move arguments into place.
            mov r4, r0
            mov r5, r1

            svc #0

            @ Copy outputs into place.
            mov r0, r4

            @ Restore the registers.
            pop {{r4-r5, r11}}
            .cfi_adjust_cfa_offset -12
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::Post as u32,
        );
    } else {
        global_asm!("
        .section .text.sys_post_stub
        .globl sys_post_stub
        .type sys_post_stub,function
        sys_post_stub:
            .cfi_startproc

            @ Stash the register values that we'll use to carry arguments.
            push {{r4-r5}}
            .cfi_adjust_cfa_offset 8
            .cfi_offset r4, -8
            .cfi_offset r5, -4

            mov r4, r11
            push {{r4}}
            .cfi_adjust_cfa_offset 4
            .cfi_offset r11, -12

            @ Materialize the sysnum constant.
            eors r4, r4
            adds r4, #{sysnum}
            mov r11, r4

            @ Move arguments into place.
            mov r4, r0
            mov r5, r1

            svc #0

            @ Copy outputs into place.
            mov r0, r4

            @ Restore the registers.
            pop {{r4}}
            .cfi_adjust_cfa_offset -4
            mov r11, r4
            pop {{r4-r5}}
            bx lr

            .cfi_endproc
        ",
            sysnum = const Sysnum::Post as u32,
        );
    }
}
extern "C" {
    /// Low-level send syscall stub.
    ///
    /// # Safety
    ///
    /// To use this safely, all of the (base,len) pointers must meet the
    /// validity rules for slice references. The easiest way to ensure this is
    /// to derive them directly from slice references.
    ///
    /// This also implies that the outgoing, incoming, and lease regions may not
    /// overlap.
    ///
    /// As an optimization, the memory pointed to by `incoming_base` need not be
    /// initialized, and so it is safe to have derived the `incoming_base`
    /// pointer from an array of `MaybeUninit<u8>`. Once this returns, you can
    /// assume that the _prefix_ of the `incoming` slice up to the response
    /// length has been initialized. The tail of that buffer may _not_ have been
    /// initialized.
    fn sys_send_stub(
        target_and_operation: u32,
        outgoing_base: *const u8,
        outgoing_len: usize,
        incoming_base: *mut u8,
        incoming_len: usize,
        lease_base: *const AbiLease,
        lease_count: usize,
    ) -> u64;

    /// Low-level recv syscall stub.
    ///
    /// # Safety
    ///
    /// To use this safely, the incoming base/len pointers must meet the
    /// validity rules for a slice reference. The easiest way to ensure this is
    /// to derive them directly from a slice reference.
    ///
    /// This also implies that the `incoming` slice and `out` pointee may not
    /// overlap.
    ///
    /// As an optimization, the memory pointed to by `out` need not be
    /// initialized, and so it is safe to have derived the `out` pointer from a
    /// `MaybeUninit<AbiRecvMessage>`. Once this returns, you can assume the
    /// memory has been initialized.
    fn sys_recv_stub(
        incoming_base: *mut u8,
        incoming_len: usize,
        notification_mask: u32,
        sender_bits: u32,
        out: *mut AbiRecvMessage,
    ) -> u32;

    /// Low-level reply syscall stub.
    ///
    /// # Safety
    ///
    /// To use this safely, the `outgoing` base/len pair must meet the validity
    /// rules for a slice reference. The easiest way to ensure this is to derive
    /// them directly from a slice reference.
    fn sys_reply_stub(
        sender: u32,
        code: u32,
        outgoing_base: *const u8,
        outgoing_len: usize,
    );

    /// Low level reply-fault syscall stub.
    ///
    /// # Safety
    ///
    /// This operation has no safety implications. It's only considered `unsafe`
    /// by Rust because it's `extern "C"`. Have fun.
    fn sys_reply_fault_stub(
        sender: u32,
        reason: u32,
    );

    /// Low-level panic syscall stub.
    ///
    /// # Safety
    ///
    /// The pointer/length passed to this function must be valid.
    ///
    /// Well, strictly speaking, they don't, at least not from Rust's
    /// perspective. They will not be dereferenced in this lifetime. They're
    /// just sent to the kernel. But, it's a good idea to use valid pointers in
    /// case somebody wants to read your string after your death.
    fn sys_panic_stub(_msg: *const u8, _len: usize) -> !;

    /// Low-level set-timer syscall stub.
    ///
    /// # Safety
    ///
    /// This is only considered "unsafe" by Rust because it's `extern "C"`.
    /// Calling this has no safety implications. Have fun.
    fn sys_set_timer_stub(
        enable: u32,
        raw_deadline_lo: u32,
        raw_deadline_hi: u32,
        notifications: u32,
    );

    /// Low-level get-timer syscall stub.
    ///
    /// # Safety
    ///
    /// This is only considered "unsafe" by Rust because it's `extern "C"`.
    /// Calling this has no safety implications. Have fun.
    fn sys_get_timer_stub() -> AbiTimerSettings;

    fn sys_irq_control_stub(bits: u32, status: u32);

    fn sys_borrow_info_stub(tid_bits: u32, index: u32, out: *mut AbiBorrowInfo);
    fn sys_borrow_read_stub(tid_bits: u32, index: u32, offset: u32, dest: *mut u8, dest_len: usize) -> u64;
    fn sys_borrow_write_stub(tid_bits: u32, index: u32, offset: u32, src: *const u8, src_len: usize) -> u64;

    fn sys_post_stub(tid_bits: u32, notification: u32) -> u32;
}

cfg_if::cfg_if! {
    if #[cfg(hubris_target = "thumbv6m-none-eabi")] {
        global_asm!("
        .section .text.start
        .globl _start
        _start:
            @ Copy data initialization image into data section.
            @ Note: this assumes that both source and destination are 32-bit
            @ aligned and padded to 4-byte boundary.

            ldr r0, =__edata            @ upper bound of output
            ldr r1, =__sidata           @ start of source image
            ldr r2, =__sdata            @ start of output

            b 1f                        @ jump to loop tail to detect empty .data

        2:  ldm r1!, {{r3}}             @ read and advance source
            stm r2!, {{r3}}             @ write and advance dest

        1:  cmp r2, r0                  @ done yet?
            bne 2b                      @ if not, repeat.

            @ Zero BSS section.

            ldr r0, =__ebss             @ upper bound of zeroed region
            ldr r1, =__sbss             @ lower bound
            movs r2, #0                 @ the all-important constant

            b 1f                        @ handle empty .bss

        2:  stm r1!, {{r2}}             @ zero one word and increment pointer
        1:  cmp r1, r0                  @ done yet?
            bne 2b                      @ no? continue.

            @ Now, to the user entry point. We call it in case it
            @ returns. (It's not supposed to.) We reference it through
            @ a sym operand because it's a Rust func and may be mangled.
            bl {main}

            @ Should main return... kill it.
            udf 0xad
            ",
            main = sym main,
        );
    } else if #[cfg(any(
        // It'd sure be nice if we could detect proper thumb2
        hubris_target = "thumbv7m-none-eabi",
        hubris_target = "thumbv7em-none-eabi",
        hubris_target = "thumbv7em-none-eabihf",
        hubris_target = "thumbv8m.main-none-eabi",
        hubris_target = "thumbv8m.main-none-eabihf",
    ))] {
        global_asm!("
        .section .text.start
        .globl _start
        _start:
            @ Copy data initialization image into data section.
            @ Note: this assumes that both source and destination are 32-bit
            @ aligned and padded to 4-byte boundary.

            movw r0, #:lower16:__edata  @ upper bound in r0
            movt r0, #:upper16:__edata

            movw r1, #:lower16:__sidata @ source in r1
            movt r1, #:upper16:__sidata

            movw r2, #:lower16:__sdata  @ dest in r2
            movt r2, #:upper16:__sdata

            b 1f                        @ check for zero-sized data

        2:  ldr r3, [r1], #4            @ read and advance source
            str r3, [r2], #4            @ write and advance dest

        1:  cmp r2, r0                  @ has dest reached the upper bound?
            bne 2b                      @ if not, repeat

            @ Zero BSS section.

            movw r0, #:lower16:__ebss   @ upper bound in r0
            movt r0, #:upper16:__ebss

            movw r1, #:lower16:__sbss   @ base in r1
            movt r1, #:upper16:__sbss

            movs r2, #0                 @ materialize a zero

            b 1f                        @ check for zero-sized BSS

        2:  str r2, [r1], #4            @ zero one word and advance

        1:  cmp r1, r0                  @ has base reached bound?
            bne 2b                      @ if not, repeat

            @ Now, to the user entry point. We call it in case it
            @ returns. (It's not supposed to.) We reference it through
            @ a sym operand because it's a Rust func and may be mangled.
            bl {main}

            @ Should main return... kill it.
            udf 0xad
            ",
            main = sym main,
        );
    } else {
        compile_error!("unrecognized target for start code");
    }
}
