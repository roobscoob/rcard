#![no_std]
#![no_main]

use core::mem::MaybeUninit;

use hubris_task_slots::SLOTS;
use sysmodule_log_api::{Log, LogDispatcher, LogLevel};

sysmodule_usart_api::bind_usart!(Usart = SLOTS.sysmodule_usart);
sysmodule_time_api::bind_time!(Time = SLOTS.sysmodule_time);

mod generated {
    include!(concat!(env!("OUT_DIR"), "/task_names.rs"));
}

static mut USART: MaybeUninit<Usart> = MaybeUninit::uninit();

fn usart_write(data: &[u8]) {
    let usart = unsafe { USART.assume_init_ref() };
    let _ = usart.write(data);
}

struct LogResource;

fn level_str(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Panic => "PANIC",
        LogLevel::Error => "ERROR",
        LogLevel::Warn => "WARN",
        LogLevel::Info => "INFO",
        LogLevel::Debug => "DEBUG",
        LogLevel::Trace => "TRACE",
    }
}

fn write_u8_pad2(val: u8) {
    usart_write(&[b'0' + val / 10, b'0' + val % 10]);
}

fn write_prefix(level: LogLevel, meta: &ipc::Meta) {
    let task_index = meta.sender.task_index() as usize;
    let task_name = generated::TASK_NAMES.get(task_index).unwrap_or(&"???");

    if let Ok(Some(dt)) = Time::get_time() {
        write_u8_pad2(dt.day);
        usart_write(b"/");
        write_u8_pad2(dt.month);
        usart_write(b"/");
        // Year: write last two digits
        write_u8_pad2((dt.year % 100) as u8);
        usart_write(b" ");
        write_u8_pad2(dt.hour);
        usart_write(b":");
        write_u8_pad2(dt.minute);
        usart_write(b":");
        write_u8_pad2(dt.second);
        usart_write(b" ");
    } else {
        usart_write(b"????/??/?? ??:??:?? ");
    }

    usart_write(b"[");
    usart_write(level_str(level).as_bytes());
    usart_write(b" ");
    usart_write(task_name.as_bytes());
    usart_write(b"] ");
}

fn write_leased(data: &idyll_runtime::Leased<idyll_runtime::Read, u8>) {
    let len = data.len();
    let mut buf = [0u8; 128];
    let mut offset = 0;
    while offset < len {
        let chunk = (len - offset).min(buf.len());
        let _ = data.read_range(offset, &mut buf[..chunk]);
        usart_write(&buf[..chunk]);
        offset += chunk;
    }
}

impl Log for LogResource {
    fn log(meta: ipc::Meta, level: LogLevel, data: idyll_runtime::Leased<idyll_runtime::Read, u8>) {
        write_prefix(level, &meta);
        write_leased(&data);
        usart_write(b"\r\n");
    }

    fn start(meta: ipc::Meta, level: LogLevel) -> Option<Self> {
        write_prefix(level, &meta);
        Some(LogResource)
    }

    fn write(&mut self, _meta: ipc::Meta, data: idyll_runtime::Leased<idyll_runtime::Read, u8>) {
        write_leased(&data);
    }
}

impl Drop for LogResource {
    fn drop(&mut self) {
        usart_write(b"\r\n");
    }
}

#[export_name = "main"]
fn main() -> ! {
    // Open USART2 for log output.
    let usart = Usart::open(2).unwrap().unwrap();
    unsafe { USART.write(usart) };

    let mut dispatcher = LogDispatcher::<LogResource>::new();
    let mut buf = [MaybeUninit::uninit(); 256];

    ipc::Server::<1>::new()
        .with_dispatcher(0x03, &mut dispatcher)
        .run(&mut buf)
}
