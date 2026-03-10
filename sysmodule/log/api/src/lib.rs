#![no_std]

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    hubpack::SerializedSize,
)]
#[repr(u8)]
pub enum LogLevel {
    Panic,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => LogLevel::Panic,
            1 => LogLevel::Error,
            2 => LogLevel::Warn,
            3 => LogLevel::Info,
            4 => LogLevel::Debug,
            _ => LogLevel::Trace,
        }
    }
}

#[cfg(feature = "logger")]
pub use log;

#[cfg(feature = "logger")]
pub trait LogBackend {
    type Handle;
    fn log_atomic(level: LogLevel, data: &[u8]);
    fn start(level: LogLevel) -> Option<Self::Handle>;
    fn write_handle(handle: &Self::Handle, data: &[u8]);
}

#[cfg(feature = "logger")]
pub struct LogWriter<B: LogBackend> {
    buffer: [u8; 128],
    buffer_offset: usize,
    handle: Option<B::Handle>,
    cancelled: bool,
    log_level: LogLevel,
}

#[cfg(feature = "logger")]
impl<B: LogBackend> LogWriter<B> {
    pub fn new(level: LogLevel) -> Self {
        Self {
            buffer: [0u8; 128],
            buffer_offset: 0,
            handle: None,
            cancelled: false,
            log_level: level,
        }
    }

    pub fn finish(&mut self) {
        if self.cancelled || self.buffer_offset == 0 {
            return;
        }
        if let Some(handle) = &self.handle {
            B::write_handle(handle, &self.buffer[..self.buffer_offset]);
        } else {
            B::log_atomic(self.log_level, &self.buffer[..self.buffer_offset]);
        }
    }

    /// Returns whatever is in the buffer (for sys_panic).
    pub fn buffer(&self) -> &[u8] {
        &self.buffer[..self.buffer_offset]
    }
}

#[cfg(feature = "logger")]
impl<B: LogBackend> core::fmt::Write for LogWriter<B> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        if self.cancelled {
            return Ok(());
        }

        let mut remaining = s.as_bytes();
        while !remaining.is_empty() {
            if self.buffer_offset == self.buffer.len() {
                if let Some(handle) = &self.handle {
                    B::write_handle(handle, &self.buffer);
                } else if let Some(handle) = B::start(self.log_level) {
                    B::write_handle(&handle, &self.buffer);
                    self.handle = Some(handle);
                } else {
                    self.cancelled = true;
                    return Ok(());
                }
                self.buffer_offset = 0;
            }

            let space = self.buffer.len() - self.buffer_offset;
            let to_write = remaining.len().min(space);
            self.buffer[self.buffer_offset..self.buffer_offset + to_write]
                .copy_from_slice(&remaining[..to_write]);
            self.buffer_offset += to_write;
            remaining = &remaining[to_write..];
        }

        Ok(())
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    hubpack::SerializedSize,
)]
pub enum LogError {
    Unauthorized,
}

#[ipc::resource(arena_size = 16, kind = 0x03)]
pub trait Log {
    #[message]
    fn log(level: LogLevel, #[lease] data: &[u8]);

    #[constructor]
    fn start(level: LogLevel) -> Option<Self>;

    #[message]
    fn write(&self, #[lease] data: &[u8]);

    #[message]
    fn consume_since(since_id: u32, #[lease] buf: &mut [u8]) -> Result<u32, LogError>;
}

/// Initialize the global `log` logger backed by the IPC Log sysmodule.
///
/// Call this early in your task's `main`, after `bind!` has created the
/// Log type alias:
///
/// ```ignore
/// sysmodule_log_api::bind!(Log = SLOTS.sysmodule_log);
/// sysmodule_log_api::init_logger!(Log);
/// ```
#[cfg(feature = "logger")]
#[macro_export]
macro_rules! panic_handler {
    ($Log:ty) => {
        #[panic_handler]
        fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
            use core::fmt::Write;

            struct Backend;
            impl $crate::LogBackend for Backend {
                type Handle = $Log;
                fn log_atomic(level: $crate::LogLevel, data: &[u8]) {
                    let _ = <$Log>::log(level, data);
                }
                fn start(level: $crate::LogLevel) -> Option<$Log> {
                    <$Log>::start(level).ok().flatten()
                }
                fn write_handle(handle: &$Log, data: &[u8]) {
                    let _ = handle.write(data);
                }
            }

            let mut w = $crate::LogWriter::<Backend>::new($crate::LogLevel::Panic);
            let _ = write!(w, "{}", info);
            w.finish();

            userlib::sys_panic(w.buffer())
        }
    };
}

#[cfg(feature = "logger")]
#[macro_export]
macro_rules! init_logger {
    ($Log:ty) => {{
        use core::fmt::Write;
        use $crate::log;

        struct IpcLogger;

        struct Backend;
        impl $crate::LogBackend for Backend {
            type Handle = $Log;
            fn log_atomic(level: $crate::LogLevel, data: &[u8]) {
                let _ = <$Log>::log(level, data);
            }
            fn start(level: $crate::LogLevel) -> Option<$Log> {
                <$Log>::start(level).ok().flatten()
            }
            fn write_handle(handle: &$Log, data: &[u8]) {
                let _ = handle.write(data);
            }
        }

        impl log::Log for IpcLogger {
            fn enabled(&self, _metadata: &log::Metadata) -> bool {
                true
            }

            fn log(&self, record: &log::Record) {
                let level = match record.level() {
                    log::Level::Error => $crate::LogLevel::Error,
                    log::Level::Warn => $crate::LogLevel::Warn,
                    log::Level::Info => $crate::LogLevel::Info,
                    log::Level::Debug => $crate::LogLevel::Debug,
                    log::Level::Trace => $crate::LogLevel::Trace,
                };

                let mut w = $crate::LogWriter::<Backend>::new(level);
                let _ = write!(w, "{}", record.args());
                w.finish();
            }

            fn flush(&self) {}
        }

        static LOGGER: IpcLogger = IpcLogger;
        log::set_logger(&LOGGER).unwrap();
        log::set_max_level(log::LevelFilter::Trace);

        log::trace!(concat!(env!("CARGO_PKG_NAME"), ": Awake"));
    }};
}
