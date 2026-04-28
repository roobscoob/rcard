//! Compact memory functions for ARM Thumb-2, replacing the bloated
//! `compiler_builtins` versions (~742 bytes each → ~80 bytes each).
//!
//! `#![no_builtins]` prevents LLVM from recognizing our byte loops and
//! replacing them with calls back to `memcpy`/`memset` (infinite recursion).

#![no_builtins]
#![no_std]

// ---------------------------------------------------------------------------
// memcpy / __aeabi_memcpy*
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn memcpy(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    __aeabi_memcpy(dest, src, n);
    dest
}

#[no_mangle]
pub unsafe extern "C" fn __aeabi_memcpy(dest: *mut u8, src: *const u8, n: usize) {
    let mut d = dest;
    let mut s = src;
    let mut remaining = n;

    // Align dest to 4 bytes
    while remaining > 0 && (d as usize & 3) != 0 {
        *d = *s;
        d = d.add(1);
        s = s.add(1);
        remaining -= 1;
    }
    copy_aligned_tail(d, s, remaining);
}

#[no_mangle]
pub unsafe extern "C" fn __aeabi_memcpy8(dest: *mut u8, src: *const u8, n: usize) {
    copy_aligned_tail(dest, src, n);
}

#[no_mangle]
pub unsafe extern "C" fn __aeabi_memcpy4(dest: *mut u8, src: *const u8, n: usize) {
    copy_aligned_tail(dest, src, n);
}

/// dest is word-aligned; src may or may not be.
#[inline(always)]
unsafe fn copy_aligned_tail(mut d: *mut u8, mut s: *const u8, mut n: usize) {
    if (s as usize & 3) == 0 {
        let mut dw = d as *mut u32;
        let mut sw = s as *const u32;
        while n >= 16 {
            let a = *sw;
            let b = *sw.add(1);
            let c = *sw.add(2);
            let d = *sw.add(3);
            *dw = a;
            *dw.add(1) = b;
            *dw.add(2) = c;
            *dw.add(3) = d;
            dw = dw.add(4);
            sw = sw.add(4);
            n -= 16;
        }
        while n >= 4 {
            *dw = *sw;
            dw = dw.add(1);
            sw = sw.add(1);
            n -= 4;
        }
        d = dw as *mut u8;
        s = sw as *const u8;
    }
    while n > 0 {
        *d = *s;
        d = d.add(1);
        s = s.add(1);
        n -= 1;
    }
}

// ---------------------------------------------------------------------------
// memmove
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn memmove(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    __aeabi_memmove(dest, src, n);
    dest
}

#[no_mangle]
pub unsafe extern "C" fn __aeabi_memmove(dest: *mut u8, src: *const u8, n: usize) {
    if (dest as usize) <= (src as usize) {
        __aeabi_memcpy(dest, src, n);
    } else {
        let mut i = n;
        while i > 0 {
            i -= 1;
            *dest.add(i) = *src.add(i);
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn __aeabi_memmove4(dest: *mut u8, src: *const u8, n: usize) {
    __aeabi_memmove(dest, src, n);
}

#[no_mangle]
pub unsafe extern "C" fn __aeabi_memmove8(dest: *mut u8, src: *const u8, n: usize) {
    __aeabi_memmove(dest, src, n);
}

// ---------------------------------------------------------------------------
// memset / __aeabi_memset* / __aeabi_memclr*
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn memset(dest: *mut u8, c: i32, n: usize) -> *mut u8 {
    __aeabi_memset(dest, n, c);
    dest
}

// Note: __aeabi_memset has (dest, n, c) arg order — NOT the same as memset.
#[no_mangle]
pub unsafe extern "C" fn __aeabi_memset(dest: *mut u8, n: usize, c: i32) {
    let byte = c as u8;
    let mut d = dest;
    let mut remaining = n;

    while remaining > 0 && (d as usize & 3) != 0 {
        *d = byte;
        d = d.add(1);
        remaining -= 1;
    }

    if remaining >= 4 {
        let word = u32::from_ne_bytes([byte, byte, byte, byte]);
        let mut dw = d as *mut u32;
        while remaining >= 4 {
            *dw = word;
            dw = dw.add(1);
            remaining -= 4;
        }
        d = dw as *mut u8;
    }

    while remaining > 0 {
        *d = byte;
        d = d.add(1);
        remaining -= 1;
    }
}

#[no_mangle]
pub unsafe extern "C" fn __aeabi_memset4(dest: *mut u8, n: usize, c: i32) {
    __aeabi_memset(dest, n, c);
}

#[no_mangle]
pub unsafe extern "C" fn __aeabi_memset8(dest: *mut u8, n: usize, c: i32) {
    __aeabi_memset(dest, n, c);
}

#[no_mangle]
pub unsafe extern "C" fn __aeabi_memclr(dest: *mut u8, n: usize) {
    __aeabi_memset(dest, n, 0);
}

#[no_mangle]
pub unsafe extern "C" fn __aeabi_memclr4(dest: *mut u8, n: usize) {
    __aeabi_memset(dest, n, 0);
}

#[no_mangle]
pub unsafe extern "C" fn __aeabi_memclr8(dest: *mut u8, n: usize) {
    __aeabi_memset(dest, n, 0);
}

// ---------------------------------------------------------------------------
// memcmp
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    let mut i = 0;
    while i < n {
        let av = *a.add(i);
        let bv = *b.add(i);
        if av != bv {
            return (av as i32) - (bv as i32);
        }
        i += 1;
    }
    0
}
