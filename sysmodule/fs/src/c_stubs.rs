//! C functions not provided by compiler-builtins but required by littlefs.
//!
//! With ARM GCC, littlefs2-sys ships string.c which provides strlen/memset/etc.
//! We only need strcpy here.

use core::ffi::{c_char, c_void};

extern "C" {
    fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void;
    fn strlen(s: *const c_char) -> usize;
}

#[no_mangle]
unsafe extern "C" fn strcpy(dst: *mut c_char, src: *const c_char) -> *mut c_char {
    unsafe { memcpy(dst as *mut c_void, src as *const c_void, strlen(src) + 1) as *mut c_char }
}
