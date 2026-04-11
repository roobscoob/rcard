#![no_std]
#![no_main]

use core::mem::MaybeUninit;

use hubris_abi::{FaultInfo, FaultSource, SchedState, TaskState};
use sifli_pac::usart::vals::M;

use generated::tasks::TASK_NAMES;

const FAULT_NOTIFICATION: u32 = 1;

/// Well-known operation code for drop reports from the reactor.
const OP_DROP_REPORT: u16 = 0xDEAD;

fn usart() -> sifli_pac::usart::Usart {
    sifli_pac::USART1
}

fn usart_init() {
    let u = usart();
    // TODO: fixme (corrupted output?)
    // BRR = 240MHz / 1000000 = 240 (0xF0)
    // u.brr().write(|w| w.0 = 0xF0);
    // BRR = 48MHz / 1000000 = 48 (0x30)
    u.brr().write(|w| w.0 = 0x30);
    // CR1: UE | TE
    u.cr1().write(|w| {
        w.set_m(M::Bit8);
        w.set_ue(true);
        w.set_te(true);
    });
}

fn usart_write_bytes(msg: &[u8]) {
    let u = usart();
    // Acquire EXR lock: reading busy==0 atomically sets it to 1.
    while u.exr().read().busy() {}
    for &b in msg {
        while !u.isr().read().txe() {}
        u.tdr().write(|w| w.0 = b as u32);
    }
    // Wait for transmission to fully complete before releasing.
    while !u.isr().read().tc() {}
    // Release EXR lock: write 1 to busy to unlock.
    u.exr().write(|w| w.set_busy(true));
}

fn usart_write_u32(mut val: u32) {
    if val == 0 {
        usart_write_bytes(b"0");
        return;
    }
    let mut buf = [0u8; 10];
    let mut i = buf.len();
    while val > 0 && i > 0 {
        i -= 1;
        // SAFETY: i is in 0..buf.len() because we check i > 0 above.
        unsafe { *buf.get_unchecked_mut(i) = b'0' + (val % 10) as u8 };
        val /= 10;
    }
    usart_write_bytes(unsafe { buf.get_unchecked(i..) });
}

fn usart_write_hex(mut val: u32) {
    usart_write_bytes(b"0x");
    if val == 0 {
        usart_write_bytes(b"0");
        return;
    }
    let mut buf = [0u8; 8];
    let mut i = buf.len();
    while val > 0 && i > 0 {
        i -= 1;
        let nib = (val & 0xF) as u8;
        // SAFETY: i is in 0..buf.len() because we check i > 0 above.
        unsafe {
            *buf.get_unchecked_mut(i) = if nib < 10 {
                b'0' + nib
            } else {
                b'a' + nib - 10
            };
        }
        val >>= 4;
    }
    usart_write_bytes(unsafe { buf.get_unchecked(i..) });
}

fn handle_drop_report(sender: userlib::TaskId, data: &[u8]) {
    let Some(fields): Option<&[u8; 11]> = data.get(..11).and_then(|s| s.try_into().ok()) else {
        usart_write_bytes(b"supervisor: malformed drop report\r\n");
        userlib::sys_reply(sender, userlib::ResponseCode::SUCCESS, &[]);
        return;
    };
    let notif_sender = u16::from_le_bytes([fields[0], fields[1]]);
    let group_id = u16::from_le_bytes([fields[2], fields[3]]);
    let code = u32::from_le_bytes([fields[4], fields[5], fields[6], fields[7]]);
    let priority = fields[8];
    let dropped_for = u16::from_le_bytes([fields[9], fields[10]]);

    let sender_name = TASK_NAMES
        .get(notif_sender as usize)
        .copied()
        .unwrap_or("?");
    let dropped_name = TASK_NAMES.get(dropped_for as usize).copied().unwrap_or("?");

    usart_write_bytes(b"supervisor: notification dropped for=");
    usart_write_u32(dropped_for as u32);
    usart_write_bytes(b"(");
    usart_write_bytes(dropped_name.as_bytes());
    usart_write_bytes(b") sender=");
    usart_write_u32(notif_sender as u32);
    usart_write_bytes(b"(");
    usart_write_bytes(sender_name.as_bytes());
    usart_write_bytes(b") group=");
    usart_write_u32(group_id as u32);
    usart_write_bytes(b" code=");
    usart_write_hex(code);
    usart_write_bytes(b" pri=");
    usart_write_u32(priority as u32);
    usart_write_bytes(b"\r\n");
    userlib::sys_reply(sender, userlib::ResponseCode::SUCCESS, &[]);
}

// NOTE: print_task_state / print_all_task_states call kipc::read_task_status
// which can panic (ssmarshal deserialization). The supervisor's
// supervisor_must_not_panic linker guard forbids any panic path, so these
// can only be used if read_task_status is rewritten to avoid panic.
#[allow(dead_code)]
fn usart_write_task_id(tid: hubris_abi::TaskId) {
    let name = TASK_NAMES.get(tid.0 as usize).copied().unwrap_or("?");
    usart_write_bytes(name.as_bytes());
}

fn usart_write_sched_state(state: &SchedState) {
    match state {
        SchedState::Stopped => usart_write_bytes(b"Stopped"),
        SchedState::Runnable => usart_write_bytes(b"Runnable"),
        SchedState::InSend(tid) => {
            usart_write_bytes(b"InSend(");
            usart_write_task_id(*tid);
            usart_write_bytes(b")");
        }
        SchedState::InReply(tid) => {
            usart_write_bytes(b"InReply(");
            usart_write_task_id(*tid);
            usart_write_bytes(b")");
        }
        SchedState::InRecv(Some(tid)) => {
            usart_write_bytes(b"InRecv(");
            usart_write_task_id(*tid);
            usart_write_bytes(b")");
        }
        SchedState::InRecv(None) => usart_write_bytes(b"InRecv(*)"),
    }
}

fn usart_write_fault_info(fault: &FaultInfo) {
    match fault {
        FaultInfo::MemoryAccess { address, source } => {
            usart_write_bytes(b"MemoryAccess(");
            match address {
                Some(a) => usart_write_hex(*a),
                None => usart_write_bytes(b"?"),
            }
            usart_write_bytes(match source {
                FaultSource::User => b", user",
                FaultSource::Kernel => b", kernel",
            });
            usart_write_bytes(b")");
        }
        FaultInfo::StackOverflow { address } => {
            usart_write_bytes(b"StackOverflow(");
            usart_write_hex(*address);
            usart_write_bytes(b")");
        }
        FaultInfo::BusError { address, source } => {
            usart_write_bytes(b"BusError(");
            match address {
                Some(a) => usart_write_hex(*a),
                None => usart_write_bytes(b"?"),
            }
            usart_write_bytes(match source {
                FaultSource::User => b", user",
                FaultSource::Kernel => b", kernel",
            });
            usart_write_bytes(b")");
        }
        FaultInfo::DivideByZero => usart_write_bytes(b"DivideByZero"),
        FaultInfo::IllegalText => usart_write_bytes(b"IllegalText"),
        FaultInfo::IllegalInstruction => usart_write_bytes(b"IllegalInstruction"),
        FaultInfo::InvalidOperation(code) => {
            usart_write_bytes(b"InvalidOperation(");
            usart_write_hex(*code);
            usart_write_bytes(b")");
        }
        FaultInfo::SyscallUsage(err) => {
            usart_write_bytes(b"SyscallUsage(");
            usart_write_u32(*err as u32);
            usart_write_bytes(b")");
        }
        FaultInfo::Panic => usart_write_bytes(b"Panic"),
        FaultInfo::Injected(tid) => {
            usart_write_bytes(b"Injected(");
            usart_write_task_id(*tid);
            usart_write_bytes(b")");
        }
        FaultInfo::FromServer(tid, reason) => {
            usart_write_bytes(b"FromServer(");
            usart_write_task_id(*tid);
            usart_write_bytes(b", ");
            usart_write_u32(*reason as u32);
            usart_write_bytes(b")");
        }
    }
}

fn usart_write_task_state(state: &TaskState) {
    match state {
        TaskState::Healthy(sched) => usart_write_sched_state(sched),
        TaskState::Faulted {
            fault,
            original_state,
        } => {
            usart_write_bytes(b"Faulted(");
            usart_write_fault_info(fault);
            usart_write_bytes(b", was ");
            usart_write_sched_state(original_state);
            usart_write_bytes(b")");
        }
    }
}

fn print_task_state(task_index: usize) {
    let name = TASK_NAMES.get(task_index).copied().unwrap_or("?");
    let state = kipc::read_task_status(task_index);

    usart_write_u32(task_index as u32);
    usart_write_bytes(b" ");
    usart_write_bytes(name.as_bytes());
    usart_write_bytes(b": ");
    usart_write_task_state(&state);
    usart_write_bytes(b"\r\n");
}

#[allow(dead_code)]
fn print_all_task_states() {
    usart_write_bytes(b"supervisor: task states:\r\n");
    for i in 0..TASK_NAMES.len() {
        print_task_state(i);
    }
}

fn handle_faults() {
    let mut next_task = 1;
    while let Some(fault_index) = kipc::find_faulted_task(next_task) {
        let fault_index = usize::from(fault_index);
        let state = {
            let mut response = [0u8; core::mem::size_of::<TaskState>()];
            userlib::sys_send_to_kernel(
                hubris_abi::Kipcnum::ReadTaskStatus as u16,
                &(fault_index as u32).to_le_bytes(),
                &mut response,
                &mut [],
            );
            ssmarshal::deserialize::<TaskState>(&response)
        };

        let name = TASK_NAMES.get(fault_index).copied().unwrap_or("?");
        usart_write_bytes(b"supervisor: task ");
        usart_write_u32(fault_index as u32);
        usart_write_bytes(b" (");
        usart_write_bytes(name.as_bytes());
        usart_write_bytes(b") ");
        match state {
            Ok((ref s, _)) => usart_write_task_state(s),
            Err(_) => usart_write_bytes(b"faulted (unknown state)"),
        }
        usart_write_bytes(b"\r\n");

        kipc::reinitialize_task(fault_index, kipc::NewState::Runnable);
        next_task = fault_index.wrapping_add(1);
    }
}

extern "C" {
    /// Linking will fail if any code path can reach panic.
    fn supervisor_must_not_panic() -> !;
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    unsafe { supervisor_must_not_panic() }
}

#[export_name = "main"]
fn main() -> ! {
    usart_init();
    usart_write_bytes(b"supervisor: Awake\r\n");

    let mut buf = [MaybeUninit::uninit(); 16];

    loop {
        match userlib::sys_recv_open(&mut buf, FAULT_NOTIFICATION) {
            userlib::MessageOrNotification::Notification(_) => {
                handle_faults();
            }
            userlib::MessageOrNotification::Message(msg) => {
                if msg.operation == OP_DROP_REPORT {
                    if let Ok(data) = msg.data {
                        handle_drop_report(msg.sender, data);
                    } else {
                        usart_write_bytes(b"supervisor: malformed drop report\r\n");
                        userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &[]);
                    }
                } else {
                    usart_write_bytes(b"supervisor: Invalid Syscall Opcode\r\n");
                    userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &[]);
                }
            }
        }
    }
}
