#![no_std]
#![no_main]

use hubris_task_slots::SLOTS;
use sysmodule_usart_api::client::UsartHandle;

#[export_name = "main"]
fn main() -> ! {
    let Ok(mut usart) = UsartHandle::open(SLOTS.sysmodule_usart, 2) else {
        loop {}
    };

    let _ = usart.write(b"Hello, world!\r\n");

    loop {}
}
