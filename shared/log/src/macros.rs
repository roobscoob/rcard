/// Log at the `Trace` level.
#[macro_export]
macro_rules! trace {
    ($($args:tt)*) => {
        $crate::__species!($crate, $crate::LogLevel::Trace, $($args)*)
    };
}

/// Log at the `Debug` level.
#[macro_export]
macro_rules! debug {
    ($($args:tt)*) => {
        $crate::__species!($crate, $crate::LogLevel::Debug, $($args)*)
    };
}

/// Log at the `Info` level.
#[macro_export]
macro_rules! info {
    ($($args:tt)*) => {
        $crate::__species!($crate, $crate::LogLevel::Info, $($args)*)
    };
}

/// Log at the `Warn` level.
#[macro_export]
macro_rules! warn {
    ($($args:tt)*) => {
        $crate::__species!($crate, $crate::LogLevel::Warn, $($args)*)
    };
}

/// Log at the `Error` level.
#[macro_export]
macro_rules! error {
    ($($args:tt)*) => {
        $crate::__species!($crate, $crate::LogLevel::Error, $($args)*)
    };
}

/// Log at `Panic` level, then panic without pulling in `core::fmt`.
///
/// This sends the full message through the binary logging pipeline via
/// `Format`, then triggers a bare `core::panic!("panic")` (no format
/// arguments, so `core::fmt` is never linked).
#[macro_export]
macro_rules! panic {
    ($($args:tt)*) => {{
        $crate::__species!($crate, $crate::LogLevel::Panic, $($args)*);
        $crate::PANIC_LOGGED.store(true, core::sync::atomic::Ordering::Relaxed);
        #[allow(clippy::panic)]
        { core::panic!("panic") }
    }};
}

/// Provide the log backend implementation for this binary.
///
/// Call once in your task's crate root. The argument must be the `Log` handle
/// type produced by `bind_log!`:
///
/// ```ignore
/// sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
/// rcard_log::bind_logger!(Log);
/// ```
///
/// This emits the `#[no_mangle]` extern "Rust" fns that `LogWriter` calls.
/// If a binary uses `info!` etc. without `bind_logger!`, you get a linker error.
#[macro_export]
macro_rules! bind_logger {
    ($backend:ty) => {
        #[no_mangle]
        fn __rcard_log_send(level: u8, species: u64, data: &[u8]) {
            let lvl = $crate::LogLevel::from_u8(level);
            if <$backend>::log(lvl, species, data).is_err() {
                // ServerDied — retry once
                let _ = <$backend>::log(lvl, species, data);
            }
        }

        #[no_mangle]
        fn __rcard_log_start(level: u8, species: u64) -> Option<u64> {
            let lvl = $crate::LogLevel::from_u8(level);
            match <$backend>::start(lvl, species) {
                Ok(Some(handle)) => {
                    let raw = handle.raw().0;
                    core::mem::forget(handle);
                    Some(raw)
                }
                Ok(None) => None,
                Err(ipc::errors::ConstructorError::ArenaFull) => None,
                Err(ipc::errors::ConstructorError::ServerDied) => {
                    // Retry once
                    match <$backend>::start(lvl, species) {
                        Ok(Some(handle)) => {
                            let raw = handle.raw().0;
                            core::mem::forget(handle);
                            Some(raw)
                        }
                        _ => None,
                    }
                }
            }
        }

        #[no_mangle]
        fn __rcard_log_write(handle: u64, data: &[u8]) {
            let h = <$backend>::from_raw(ipc::RawHandle(handle));
            let _ = h.write(data);
            core::mem::forget(h);
        }

        #[no_mangle]
        fn __rcard_log_end(handle: u64) {
            let h = <$backend>::from_raw(ipc::RawHandle(handle));
            drop(h);
        }
    };
}
