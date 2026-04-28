/// Log at the `Trace` level.
#[macro_export]
macro_rules! trace {
    ($($args:tt)*) => {
        $crate::__species!($crate, $crate::LogLevel::Trace, false, $($args)*)
    };
}

/// Log at the `Debug` level.
#[macro_export]
macro_rules! debug {
    ($($args:tt)*) => {
        $crate::__species!($crate, $crate::LogLevel::Debug, false, $($args)*)
    };
}

/// Log at the `Info` level.
#[macro_export]
macro_rules! info {
    ($($args:tt)*) => {
        $crate::__species!($crate, $crate::LogLevel::Info, false, $($args)*)
    };
}

/// Log at the `Warn` level.
#[macro_export]
macro_rules! warn {
    ($($args:tt)*) => {
        $crate::__species!($crate, $crate::LogLevel::Warn, false, $($args)*)
    };
}

/// Log at the `Error` level.
#[macro_export]
macro_rules! error {
    ($($args:tt)*) => {
        $crate::__species!($crate, $crate::LogLevel::Error, false, $($args)*)
    };
}

/// Log at `Panic` level, then panic without pulling in `core::fmt`.
///
/// This sends the full message through the binary logging pipeline via
/// `Format`, then triggers a bare `core::panic!("panic")` (no format
/// arguments, so `core::fmt` is never linked).
#[macro_export]
#[cfg(feature = "panics-emit-stack-dump")]
macro_rules! panic {
    ($($args:tt)*) => {{
        $crate::__species!($crate, $crate::LogLevel::Panic, true, $($args)*);
        $crate::PANIC_LOGGED.store(true, core::sync::atomic::Ordering::Relaxed);
        #[allow(clippy::panic)]
        { core::panic!("panic") }
    }};
}

#[macro_export]
#[cfg(not(feature = "panics-emit-stack-dump"))]
macro_rules! panic {
    ($($args:tt)*) => {{
        $crate::__species!($crate, $crate::LogLevel::Panic, false, $($args)*);
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
        #[cold]
        fn __rcard_log_start(level: u8, species: u64) -> Option<u64> {
            fn attempt(
                lvl: $crate::LogLevel,
                species: u64,
            ) -> core::result::Result<Option<u64>, ()> {
                match <$backend>::start(lvl, species) {
                    Ok(Some(handle)) => {
                        let raw = handle.raw().0;
                        core::mem::forget(handle);
                        Ok(Some(raw))
                    }
                    Ok(None) => Ok(None),
                    Err(ipc::errors::ConstructorError::ArenaFull) => Ok(None),
                    Err(ipc::errors::ConstructorError::ServerDied) => Err(()),
                }
            }
            let lvl = $crate::LogLevel::from_u8(level);
            match attempt(lvl, species) {
                Ok(v) => v,
                Err(()) => match attempt(lvl, species) {
                    Ok(v) => v,
                    Err(()) => None,
                },
            }
        }

        #[no_mangle]
        #[cold]
        fn __rcard_log_write(handle: u64, data: &[u8]) {
            let h = <$backend>::from_raw(ipc::RawHandle(handle));
            let _ = h.write(data);
            core::mem::forget(h);
        }

        #[no_mangle]
        #[cold]
        fn __rcard_log_end(handle: u64) {
            let h = <$backend>::from_raw(ipc::RawHandle(handle));
            drop(h);
        }
    };
}
