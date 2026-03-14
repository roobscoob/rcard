use super::{Format, Formatter, Writer};

macro_rules! impl_format {
    ($ty:ty, $method:ident) => {
        impl Format for $ty {
            #[inline]
            fn format<W: Writer>(&self, f: &mut Formatter<W>) {
                f.$method(*self);
            }
        }
    };
}

impl_format!(u8, write_u8);
impl_format!(i8, write_i8);
impl_format!(u16, write_u16);
impl_format!(i16, write_i16);
impl_format!(u32, write_u32);
impl_format!(i32, write_i32);
impl_format!(u64, write_u64);
impl_format!(i64, write_i64);
impl_format!(u128, write_u128);
impl_format!(i128, write_i128);
impl_format!(f32, write_f32);
impl_format!(f64, write_f64);
impl_format!(char, write_char);
impl_format!(bool, write_bool);

impl Format for usize {
    #[inline]
    fn format<W: Writer>(&self, f: &mut Formatter<W>) {
        f.write_u64(*self as u64);
    }
}

impl Format for isize {
    #[inline]
    fn format<W: Writer>(&self, f: &mut Formatter<W>) {
        f.write_i64(*self as i64);
    }
}

impl Format for &str {
    #[inline]
    fn format<W: Writer>(&self, f: &mut Formatter<W>) {
        f.write_str(self);
    }
}

impl Format for () {
    #[inline]
    fn format<W: Writer>(&self, f: &mut Formatter<W>) {
        f.write_unit();
    }
}

impl<T: Format, const N: usize> Format for [T; N] {
    #[inline]
    fn format<W: Writer>(&self, f: &mut Formatter<W>) {
        f.write_array(self);
    }
}

impl<T: Format> Format for [T] {
    #[inline]
    fn format<W: Writer>(&self, f: &mut Formatter<W>) {
        f.write_slice(self);
    }
}

#[cfg(feature = "userlib")]
impl Format for userlib::TaskId {
    #[inline]
    fn format<W: Writer>(&self, f: &mut Formatter<W>) {
        f.write_u16(u16::from(*self));
    }
}
