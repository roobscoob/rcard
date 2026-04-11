#![no_std]
#![no_main]
#![allow(clippy::unwrap_used)]

use generated::slots::SLOTS;
use once_cell::OnceCell;
use sysmodule_log_api::*;

mod ringbuf;
mod server;

sysmodule_usart_api::bind_usart!(Usart = SLOTS.sysmodule_usart);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

static USART: OnceCell<Usart> = OnceCell::new();

pub(crate) fn usart_write(data: &[u8]) {
    if let Some(usart) = USART.get() {
        let _ = usart.write(data);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    ipc::notify_dead!(Reactor);
    userlib::sys_panic(b"log panic")
}

#[export_name = "main"]
fn main() -> ! {
    let usart = Usart::open(2).unwrap().unwrap();
    USART.set(usart).ok();

    ipc::server! {
        Log: server::LogResource,
    }
}
