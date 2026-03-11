#![no_std]

pub use rcard_log::LogLevel;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, hubpack::SerializedSize,
)]
pub enum LogError {
    Unauthorized,
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
    fn consume_since(since_id: u32, #[lease] buf: &mut [u8]) -> Result<u32, LogError>;
}

/// Install a `#[panic_handler]` that logs panics via the IPC Log sysmodule
/// and optionally notifies servers to clean up handles before dying.
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
            // TODO: format panic info and send to log sysmodule
            ipc::notify_dead!($($Server),*);

            userlib::sys_panic(b"panic")
        }
    };
}
