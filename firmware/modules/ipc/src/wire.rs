//! Thin helpers over zerocopy for IPC serialization.
//!
//! The codegen emits inline calls to these rather than going through
//! hubpack/serde. Each compiles to a bounds check + memcpy.

use core::mem::MaybeUninit;

/// Read a value from the front of `buf`, returning it and the remaining bytes.
///
/// Used by generated dispatch code to deserialize each argument sequentially.
#[inline(always)]
pub fn read<T: zerocopy::TryFromBytes + zerocopy::KnownLayout + zerocopy::Immutable>(
    buf: &[u8],
) -> Option<(T, &[u8])> {
    zerocopy::TryFromBytes::try_read_from_prefix(buf).ok()
}

/// Write a value's bytes into `buf`, returning the number of bytes written.
///
/// Used by generated client code to serialize each argument sequentially.
#[inline(always)]
pub fn write<T: zerocopy::IntoBytes + zerocopy::Immutable>(buf: &mut [u8], val: &T) -> usize {
    let bytes = zerocopy::IntoBytes::as_bytes(val);
    buf[..bytes.len()].copy_from_slice(bytes);
    bytes.len()
}

/// Write a value's bytes into an uninitialized buffer, returning the number
/// of bytes written. No zeroing required — only the written bytes are touched.
#[inline(always)]
pub fn write_uninit<T: zerocopy::IntoBytes + zerocopy::Immutable>(
    buf: &mut [MaybeUninit<u8>],
    val: &T,
) -> usize {
    let bytes = zerocopy::IntoBytes::as_bytes(val);
    let len = bytes.len();
    // SAFETY: MaybeUninit<u8> has the same layout as u8.
    // We're writing initialized bytes into the buffer.
    unsafe {
        core::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            buf.as_mut_ptr() as *mut u8,
            len,
        );
    }
    len
}

/// Set a single byte in an uninitialized buffer.
#[inline(always)]
pub fn set_uninit(buf: &mut [MaybeUninit<u8>], index: usize, value: u8) {
    buf[index] = MaybeUninit::new(value);
}

/// Get a `&[u8]` view of the first `len` elements of an initialized
/// `MaybeUninit` buffer.
///
/// # Safety
///
/// The caller must ensure that `buf[..len]` has been fully initialized.
#[inline(always)]
pub unsafe fn assume_init_slice(buf: &[MaybeUninit<u8>], len: usize) -> &[u8] {
    unsafe { core::slice::from_raw_parts(buf.as_ptr() as *const u8, len) }
}

/// Get a `&mut [u8]` view of a `MaybeUninit<u8>` buffer for syscall receive
/// buffers where the kernel writes data before the caller reads it.
///
/// # Safety
///
/// The caller must only read bytes that have been initialized (e.g. by
/// a preceding syscall that reports how many bytes were written).
#[inline(always)]
pub unsafe fn as_mut_byte_slice(buf: &mut [MaybeUninit<u8>]) -> &mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len()) }
}
