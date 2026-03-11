use sysmodule_log_api::LogLevel;

use crate::Time;

pub fn level_str(level: LogLevel) -> &'static str {
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

pub fn write_timestamp<F: FnMut(&[u8])>(mut out: F) {
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
}

pub fn write_tag<F: FnMut(&[u8])>(level: LogLevel, task_name: &str, mut out: F) {
    out(b"[");
    out(level_str(level).as_bytes());
    out(b" ");
    out(task_name.as_bytes());
    out(b"] ");
}

pub fn write_prefix_to<F: FnMut(&[u8])>(level: LogLevel, task_name: &str, mut out: F) {
    write_timestamp(&mut out);
    write_tag(level, task_name, &mut out);
}

#[allow(dead_code)]
pub fn read_leased_chunks(
    data: &ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
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
