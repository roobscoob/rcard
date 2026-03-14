use crate::formatter::Format;

pub trait ResultExt<T, E> {
    fn log_unwrap(self) -> T
    where
        E: Format;

    fn log_expect(self, msg: &str) -> T
    where
        E: Format;
}

impl<T, E> ResultExt<T, E> for Result<T, E> {
    #[track_caller]
    #[inline]
    fn log_unwrap(self) -> T
    where
        E: Format,
    {
        match self {
            Ok(v) => v,
            Err(e) => crate::panic!("unwrap called on Err: {}", e),
        }
    }

    #[track_caller]
    #[inline]
    fn log_expect(self, msg: &str) -> T
    where
        E: Format,
    {
        match self {
            Ok(v) => v,
            Err(e) => crate::panic!("{}: {}", msg, e),
        }
    }
}

pub trait OptionExt<T> {
    fn log_unwrap(self) -> T;
    fn log_expect(self, msg: &str) -> T;
}

impl<T> OptionExt<T> for Option<T> {
    #[track_caller]
    #[inline]
    fn log_unwrap(self) -> T {
        match self {
            Some(v) => v,
            None => crate::panic!("unwrap called on None"),
        }
    }

    #[track_caller]
    #[inline]
    fn log_expect(self, msg: &str) -> T {
        match self {
            Some(v) => v,
            None => crate::panic!("{}", msg),
        }
    }
}
