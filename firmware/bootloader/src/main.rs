#![no_std]
#![no_main]

use core::ptr;

core::arch::global_asm!(
    ".section .text.start, \"ax\"",
    ".global _start",
    ".thumb_func",
    "_start:",
    "ldr sp, =_stack_start",
    "bl bootloader_main",
    "b .",
);

const USART1_BASE: u32 = 0x5008_4000;
const ISR: *const u32 = (USART1_BASE + 0x1C) as *const u32;
const TDR: *mut u32 = (USART1_BASE + 0x28) as *mut u32;
const EXR: *mut u32 = (USART1_BASE + 0x38) as *mut u32;

fn usart1_write_bytes(bytes: &[u8]) {
    const TXE: u32 = 1 << 7;
    const TC: u32 = 1 << 6;

    unsafe {
        // Acquire EXR_BUSY to avoid conflicts with the debug interface.
        while ptr::read_volatile(EXR) & 1 != 0 {}
        ptr::write_volatile(EXR, 1);

        for &b in bytes {
            while ptr::read_volatile(ISR) & TXE == 0 {}
            ptr::write_volatile(TDR, b as u32);
        }

        // Wait for transmission complete before releasing.
        while ptr::read_volatile(ISR) & TC == 0 {}
        ptr::write_volatile(EXR, 0);
    }
}

#[no_mangle]
pub unsafe extern "C" fn bootloader_main() -> ! {
    usart1_write_bytes(b"bootloader: Awake\r\n");

    loop {
        cortex_m::asm::wfi();
    }
}

extern "C" {
    /// Linking will fail if any code path can reach panic.
    fn bootloader_must_not_panic() -> !;
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    unsafe { bootloader_must_not_panic() }
}
