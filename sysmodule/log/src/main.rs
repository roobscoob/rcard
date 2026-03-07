#![no_std]
#![no_main]

use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

use hubris_task_slots::SLOTS;
use ipc::Meta;
use sysmodule_log_api::*;
use sysmodule_storage_api::ring::RingWriter;
use userlib::TaskId;

sysmodule_usart_api::bind_usart!(Usart = SLOTS.sysmodule_usart);
sysmodule_time_api::bind_time!(Time = SLOTS.sysmodule_time);
sysmodule_storage_api::bind_partition!(StoragePartition = SLOTS.sysmodule_storage);

mod generated {
    include!(concat!(env!("OUT_DIR"), "/task_names.rs"));
}

static mut USART: MaybeUninit<Usart> = MaybeUninit::uninit();
static USART_READY: AtomicBool = AtomicBool::new(false);

fn usart_write(data: &[u8]) {
    let usart = unsafe { USART.assume_init_ref() };
    let _ = usart.write(data);
}

// ── Sinks ──────────────────────────────────────────────────────────

struct UartSink;

impl UartSink {
    const MIN_LEVEL: LogLevel = LogLevel::Trace;

    fn accepts(&self, level: LogLevel) -> bool {
        (level as u8) <= (Self::MIN_LEVEL as u8)
    }

    fn write(&self, data: &[u8]) {
        usart_write(data);
    }
}

struct RingSink {
    writer: RingWriter,
}

impl RingSink {
    const MIN_LEVEL: LogLevel = LogLevel::Info;

    fn accepts(&self, level: LogLevel) -> bool {
        (level as u8) <= (Self::MIN_LEVEL as u8)
    }

    fn begin(&mut self) {
        self.writer.begin();
    }

    fn write(&mut self, data: &[u8]) {
        self.writer.write(data);
    }

    fn end(&mut self) {
        self.writer.end();
    }
}

// ── Globals ────────────────────────────────────────────────────────

static mut RING_SINK: Option<RingSink> = None;

fn with_ring<F: FnOnce(&mut RingSink)>(f: F) {
    unsafe {
        if let Some(ref mut ring) = RING_SINK {
            f(ring);
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────

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

fn fmt_u8_pad2(val: u8) -> [u8; 2] {
    [b'0' + val / 10, b'0' + val % 10]
}

fn write_prefix_to<F: FnMut(&[u8])>(level: LogLevel, task_name: &str, mut out: F) {
    if let Ok(Some(dt)) = Time::get_time() {
        out(&fmt_u8_pad2(dt.day));
        out(b"/");
        out(&fmt_u8_pad2(dt.month));
        out(b"/");
        out(&fmt_u8_pad2((dt.year % 100) as u8));
        out(b" ");
        out(&fmt_u8_pad2(dt.hour));
        out(b":");
        out(&fmt_u8_pad2(dt.minute));
        out(b":");
        out(&fmt_u8_pad2(dt.second));
        out(b" ");
    } else {
        out(b"????/??/?? ??:??:?? ");
    }

    out(b"[");
    out(level_str(level).as_bytes());
    out(b" ");
    out(task_name.as_bytes());
    out(b"] ");
}

fn read_leased_chunks(
    data: &idyll_runtime::Leased<idyll_runtime::Read, u8>,
    mut f: impl FnMut(&[u8]),
) {
    let len = data.len();
    let mut buf = [0u8; 128];
    let mut offset = 0;
    while offset < len {
        let chunk = (len - offset).min(buf.len());
        let _ = data.read_range(offset, &mut buf[..chunk]);
        f(&buf[..chunk]);
        offset += chunk;
    }
}

// ── Server ─────────────────────────────────────────────────────────

struct LogResource {
    uart_active: bool,
    ring_active: bool,
}

impl Log for LogResource {
    fn log(meta: Meta, level: LogLevel, data: idyll_runtime::Leased<idyll_runtime::Read, u8>) {
        let task_index = meta.sender.task_index() as usize;
        let task_name = generated::TASK_NAMES.get(task_index).unwrap_or(&"???");

        let uart = UartSink;
        let use_uart = uart.accepts(level);
        let use_ring = unsafe { RING_SINK.as_ref().map_or(false, |r| r.accepts(level)) };

        if !use_uart && !use_ring {
            return;
        }

        if use_ring {
            with_ring(|r| {
                r.begin();
                write_prefix_to(level, task_name, |d| r.write(d));
            });
        }

        if use_uart {
            write_prefix_to(level, task_name, |d| usart_write(d));
        }

        read_leased_chunks(&data, |chunk| {
            if use_uart {
                usart_write(chunk);
            }
            if use_ring {
                with_ring(|r| r.write(chunk));
            }
        });

        if use_uart {
            usart_write(b"\r\n");
        }

        if use_ring {
            with_ring(|r| r.end());
        }
    }

    fn start(meta: Meta, level: LogLevel) -> Option<Self> {
        let task_index = meta.sender.task_index() as usize;
        let task_name = generated::TASK_NAMES.get(task_index).unwrap_or(&"???");

        let uart = UartSink;
        let uart_active = uart.accepts(level);
        let ring_active = unsafe { RING_SINK.as_ref().map_or(false, |r| r.accepts(level)) };

        if !uart_active && !ring_active {
            return None;
        }

        if uart_active {
            write_prefix_to(level, task_name, |d| usart_write(d));
        }

        if ring_active {
            with_ring(|r| {
                r.begin();
                write_prefix_to(level, task_name, |d| r.write(d));
            });
        }

        Some(LogResource {
            uart_active,
            ring_active,
        })
    }

    fn write(&mut self, _meta: Meta, data: idyll_runtime::Leased<idyll_runtime::Read, u8>) {
        read_leased_chunks(&data, |chunk| {
            if self.uart_active {
                usart_write(chunk);
            }
            if self.ring_active {
                with_ring(|r| r.write(chunk));
            }
        });
    }
}

impl Drop for LogResource {
    fn drop(&mut self) {
        if self.uart_active {
            usart_write(b"\r\n");
        }
        if self.ring_active {
            with_ring(|r| r.end());
        }
    }
}

// ── Panic handler ──────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if USART_READY.load(Ordering::Acquire) {
        use core::fmt::Write;
        struct PanicWriter;
        impl Write for PanicWriter {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                usart_write(s.as_bytes());
                Ok(())
            }
        }
        usart_write(b"\r\n\r\n????/??/?? ??:??:?? [PANIC sysmodule_log] ");
        let _ = write!(PanicWriter, "{}", info);
        usart_write(b"\r\n");
    }
    userlib::sys_panic(b"log panic")
}

// ── Entry point ────────────────────────────────────────────────────

#[export_name = "main"]
fn main() -> ! {
    // Open USART2 for log output.
    let usart = Usart::open(2).unwrap().unwrap();
    unsafe { USART.write(usart) };
    USART_READY.store(true, Ordering::Release);

    write_prefix_to(LogLevel::Trace, "sysmodule_log", |d| usart_write(d));
    usart_write(b"sysmodule_log: Awake\r\n");

    // Try to initialize the ring buffer sink.
    match StoragePartition::acquire(sysmodule_storage_api::partitions::LOGS) {
        Ok(Ok(handle)) => {
            let storage = storage_api::StorageDyn::from_dyn_handle(handle.into());
            let writer = RingWriter::new(storage);
            unsafe { RING_SINK = Some(RingSink { writer }) };

            write_prefix_to(LogLevel::Info, "sysmodule_log", |d| usart_write(d));
            usart_write(b"Ring buffer sink initialized\r\n");
        }
        _ => {
            write_prefix_to(LogLevel::Warn, "sysmodule_log", |d| usart_write(d));
            usart_write(b"Failed to acquire logs partition, ring sink disabled\r\n");
        }
    }

    ipc::server! {
        Log: LogResource,
    }
}
