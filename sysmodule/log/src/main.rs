#![no_std]
#![no_main]

use hubris_task_slots::SLOTS;
use once_cell::OnceCell;
use sysmodule_log_api::*;

mod fmt;
mod ringbuf;
mod server;

sysmodule_usart_api::bind_usart!(Usart = SLOTS.sysmodule_usart);
sysmodule_time_api::bind_time!(Time = SLOTS.sysmodule_time);

sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

mod generated {
    include!(concat!(env!("OUT_DIR"), "/task_names.rs"));
    include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
}

static USART: OnceCell<Usart> = OnceCell::new();

pub(crate) fn usart_write(data: &[u8]) {
    if let Some(usart) = USART.get() {
        let _ = usart.write(data);
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if USART.get().is_some() {
        use core::fmt::Write;
        struct PanicWriter;
        impl Write for PanicWriter {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                usart_write(s.as_bytes());
                Ok(())
            }
        }
        usart_write(b"\r\n\r\n");
        fmt::write_prefix_to(LogLevel::Panic, "sysmodule_log", |d| usart_write(d));
        let _ = write!(PanicWriter, "{}", info);
        usart_write(b"\r\n");
    }
    ipc::notify_dead!(Time, Reactor);
    userlib::sys_panic(b"log panic")
}

#[export_name = "main"]
fn main() -> ! {
    let usart = Usart::open(2).unwrap().unwrap();
    USART.set(usart).ok();

    fmt::write_prefix_to(LogLevel::Trace, "sysmodule_log", |d| usart_write(d));
    usart_write(b"sysmodule_log: Awake\r\n");

    ipc::server! {
        Log: server::LogResource,
    }
}
