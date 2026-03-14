#![no_std]
#![no_main]

use core::mem::MaybeUninit;

include!(concat!(env!("OUT_DIR"), "/task_names.rs"));

const FAULT_NOTIFICATION: u32 = 1;

/// Well-known operation code for drop reports from the reactor.
const OP_DROP_REPORT: u16 = 0xDEAD;

fn usart_init() {
    unsafe {
        let base = 0x5008_4000u32 as *mut u32;
        base.byte_add(0x0C).write_volatile(0x1A1); // BRR = 48MHz / 115200
        base.byte_add(0x00).write_volatile(0b1001); // CR1: UE | TE
    }
}

fn usart_write_bytes(msg: &[u8]) {
    unsafe {
        let base = 0x5008_4000u32 as *mut u32;
        for &b in msg {
            while base.byte_add(0x1C).read_volatile() & (1 << 7) == 0 {}
            base.byte_add(0x28).write_volatile(b as u32);
        }
    }
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

fn handle_faults() {
    let mut next_task = 1;
    while let Some(fault_index) = kipc::find_faulted_task(next_task) {
        let fault_index = usize::from(fault_index);

        let name = TASK_NAMES.get(fault_index).copied().unwrap_or("?");

        usart_write_bytes(b"supervisor: task ");
        usart_write_u32(fault_index as u32);
        usart_write_bytes(b" (");
        usart_write_bytes(name.as_bytes());
        usart_write_bytes(b") faulted\r\n");

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
