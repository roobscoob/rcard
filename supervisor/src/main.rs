#![no_std]
#![no_main]

use core::fmt::Write;

const FAULT_NOTIFICATION: u32 = 1;

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

struct UsartWriter;

impl Write for UsartWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        usart_write_bytes(s.as_bytes());
        Ok(())
    }
}

#[export_name = "main"]
fn main() -> ! {
    usart_init();
    usart_write_bytes(b"[super] started\r\n");

    loop {
        userlib::sys_recv_notification(FAULT_NOTIFICATION);

        let mut next_task = 1;
        while let Some(fault_index) = kipc::find_faulted_task(next_task) {
            let fault_index = usize::from(fault_index);
            let state = kipc::read_task_status(fault_index);

            let mut w = UsartWriter;
            let _ = write!(w, "[super] task {} faulted: {:?}\r\n", fault_index, state);

            kipc::reinitialize_task(fault_index, kipc::NewState::Runnable);
            next_task = fault_index.wrapping_add(1);
        }
    }
}
