//! Bounds-checked RAM slice accessor for block operations.
//!
//! Ported verbatim from sifli-rs `sifli-hal/src/ram/ram_slice.rs` @
//! commit aa4c19c. License: Apache-2.0 (upstream).
//! See `LICENSES/SIFLI-RS-APACHE-2.0.txt` for the full upstream notice.
//!
//! Adaptations from upstream:
//! - None — this is a verbatim port. The module is small and self-contained,
//!   no sifli-hal dependencies of its own. Used pervasively by the vendored
//!   `rf_cal/*` files for typed table writes into RFC SRAM and Exchange
//!   Memory clears.
//!
//! Note on `debug_assert!`: workspace clippy denies `panic`, but
//! `debug_assert!` compiles to a no-op in release builds and to a debug
//! assertion (which routes through the panic handler) in debug builds.
//! That's the same convention `circular_buf.rs` and friends already use.

/// A slice of RAM defined by base address and length in bytes.
///
/// Wraps a raw address + length pair, providing bounds-checked block
/// operations. All `unsafe` pointer access is confined within these
/// methods.
#[derive(Debug, Clone, Copy)]
pub struct RamSlice {
    addr: usize,
    len: usize,
}

impl RamSlice {
    /// Create a new RAM slice.
    ///
    /// # Safety contract
    ///
    /// Caller must ensure `addr..addr+len` is a valid, writable memory
    /// range (e.g. LCPU shared memory, Exchange Memory, NVDS buffer)
    /// reachable from the current task's MPU configuration.
    #[inline]
    pub const fn new(addr: usize, len: usize) -> Self {
        Self { addr, len }
    }

    /// Base address.
    #[inline]
    pub const fn addr(&self) -> usize {
        self.addr
    }

    /// Length in bytes.
    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear the entire region to zero.
    #[inline]
    pub fn clear(&self) {
        unsafe {
            core::ptr::write_bytes(self.addr as *mut u8, 0, self.len);
        }
    }

    /// Copy a byte slice into this region at offset 0.
    #[inline]
    pub fn copy_from_slice(&self, data: &[u8]) {
        debug_assert!(
            data.len() <= self.len,
            "RamSlice copy overflow: {} bytes into {} byte region",
            data.len(),
            self.len,
        );
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.addr as *mut u8, data.len());
        }
    }

    /// Copy a byte slice into this region at the given byte offset.
    #[inline]
    pub fn copy_at(&self, offset: usize, data: &[u8]) {
        debug_assert!(
            offset + data.len() <= self.len,
            "RamSlice copy_at overflow: offset {} + {} bytes > len {}",
            offset,
            data.len(),
            self.len,
        );
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr(),
                (self.addr + offset) as *mut u8,
                data.len(),
            );
        }
    }

    /// Read a value at the given byte offset using `read_volatile`.
    #[inline]
    pub fn read<T: Copy>(&self, offset: usize) -> T {
        debug_assert!(
            offset + core::mem::size_of::<T>() <= self.len,
            "RamSlice read overflow: offset {} + {} bytes > len {}",
            offset,
            core::mem::size_of::<T>(),
            self.len,
        );
        let addr = self.addr + offset;
        debug_assert!(
            addr % core::mem::align_of::<T>() == 0,
            "RamSlice: unaligned read at {:#x} (required alignment: {})",
            addr,
            core::mem::align_of::<T>(),
        );
        unsafe { core::ptr::read_volatile(addr as *const T) }
    }

    /// Create a sub-slice at `offset` bytes with `len` bytes.
    #[inline]
    pub fn slice(&self, offset: usize, len: usize) -> Self {
        debug_assert!(
            offset + len <= self.len,
            "RamSlice slice overflow: offset {} + {} > len {}",
            offset,
            len,
            self.len,
        );
        Self {
            addr: self.addr + offset,
            len,
        }
    }

    /// Write a value at the given byte offset using `write_volatile`.
    #[inline]
    pub fn write<T: Copy>(&self, offset: usize, value: T) {
        debug_assert!(
            offset + core::mem::size_of::<T>() <= self.len,
            "RamSlice write overflow: offset {} + {} bytes > len {}",
            offset,
            core::mem::size_of::<T>(),
            self.len,
        );
        let addr = self.addr + offset;
        debug_assert!(
            addr % core::mem::align_of::<T>() == 0,
            "RamSlice: unaligned write at {:#x} (required alignment: {})",
            addr,
            core::mem::align_of::<T>(),
        );
        unsafe {
            core::ptr::write_volatile(addr as *mut T, value);
        }
    }
}
