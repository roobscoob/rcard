#![no_std]

pub use rcard_log::LogLevel;

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum LogError {
    Unauthorized = 0,
}

#[ipc::resource(arena_size = 16, kind = 0x03)]
pub trait Log {
    #[message]
    fn log(level: LogLevel, species: u64, #[lease] argument_stream: &[u8]);

    #[constructor]
    fn start(level: LogLevel, species: u64) -> Option<Self>;

    #[message]
    fn write(&self, #[lease] data: &[u8]);

    #[message]
    fn consume_since(since_id: u64, #[lease] buf: &mut [u8]) -> Result<u32, LogError>;
}

/// Install a `#[panic_handler]` that cleans up IPC handles and terminates.
///
/// If `rcard_log::panic!` was used, the message was already logged through the
/// structured binary pipeline — the handler just cleans up and terminates.
///
/// If a bare `core::panic!("literal")` or compiler-inserted panic fires
/// (bounds checks, etc.), the handler sends the payload string through
/// `__rcard_log_send` with `species = 0` before terminating.
///
/// ```ignore
/// sysmodule_log_api::panic_handler!(to Log);
/// sysmodule_log_api::panic_handler!(to Log ; cleanup Time, Reactor);
/// ```
#[macro_export]
macro_rules! panic_handler {
    // Backwards-compat: `panic_handler!(Log)` → no cleanup servers.
    ($Log:ty) => {
        $crate::panic_handler!(to $Log ; cleanup);
    };
    (to $Log:ty) => {
        $crate::panic_handler!(to $Log ; cleanup);
    };
    (to $Log:ty ; cleanup $($Server:ty),* $(,)?) => {
        #[panic_handler]
        fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
            // If rcard_log::panic! already logged through the structured pipeline,
            // skip duplicate logging.
            if !rcard_log::PANIC_LOGGED.load(core::sync::atomic::Ordering::Relaxed) {
                // Bare core::panic! or compiler-inserted panic — send through
                // the structured pipeline as a Format-encoded string value.
                if let Some(msg) = _info.message().as_str() {
                    let mut writer = rcard_log::LogWriter::new(rcard_log::LogLevel::Panic, 0);
                    let mut f = rcard_log::formatter::Formatter::new(&mut writer);
                    rcard_log::formatter::Format::format(&msg, &mut f);
                    // LogWriter's drop closes the stream; the log server
                    // appends TAG_END_OF_STREAM on the wire.
                }
            }

            ipc::notify_dead!($($Server),*);
            userlib::sys_panic(b"panic")
        }
    };
}
