#![no_std]
#![no_main]

use core::ptr;

use rcard_places::ParseError;

const FTAB_ADDR: u32 = 0x1200_0000;

const FTAB_SLOT_PLACES: usize = 14;

core::arch::global_asm!(
    ".section .text.start, \"ax\"",
    ".balign 4",
    // Vector table: run_img loads SP from word 0 and PC from word 4.
    // .thumb_func on _reset makes the linker set bit 0 automatically.
    ".global _start",
    "_start:",
    ".word _stack_start",
    ".word _reset",
    ".thumb_func",
    "_reset:",
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
        while ptr::read_volatile(EXR) & 1 != 0 {}
        ptr::write_volatile(EXR, 1);

        for &b in bytes {
            while ptr::read_volatile(ISR) & TXE == 0 {}
            ptr::write_volatile(TDR, b as u32);
        }

        while ptr::read_volatile(ISR) & TC == 0 {}
        ptr::write_volatile(EXR, 0);
    }
}

const TICK_ZERO: &[u8; 18] = b"T0000000000000000 ";

fn usart1_write_hex(val: u32) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = [0u8; 10];
    buf[0] = b'0';
    buf[1] = b'x';
    let mut i = 2;
    let mut shift = 28i32;
    while shift >= 0 {
        buf[i] = HEX[((val >> shift) & 0xF) as usize];
        i += 1;
        shift -= 4;
    }
    usart1_write_bytes(&buf);
}

/// Read ftab[slot].base and ftab[slot].size from flash.
/// ftab layout: magic(4) + entries[16] where each entry is {base, size, xip_base, flags} (16B).
fn read_ftab_slot(slot: usize) -> (u32, u32) {
    let entry_addr = FTAB_ADDR + 4 + (slot as u32) * 16;
    unsafe {
        let base = ptr::read_volatile(entry_addr as *const u32);
        let size = ptr::read_volatile((entry_addr + 4) as *const u32);
        (base, size)
    }
}

#[no_mangle]
pub unsafe extern "C" fn bootloader_main() -> ! {
    usart1_write_bytes(TICK_ZERO);
    usart1_write_bytes(b"bootloader: awake\r\n");

    let (places_base, places_size) = read_ftab_slot(FTAB_SLOT_PLACES);
    let places_data = core::slice::from_raw_parts(places_base as *const u8, places_size as usize);

    let image = match rcard_places::PlacesImage::parse(places_data) {
        Ok(img) => img,
        Err(e) => {
            usart1_write_bytes(TICK_ZERO);
            usart1_write_bytes(b"bootloader: bad places.bin: ");

            match e {
                ParseError::TooSmall => {
                    usart1_write_bytes(b"ParseError::TooSmall\r\n");
                }
                ParseError::BadMagic => {
                    usart1_write_bytes(b"ParseError::BadMagic\r\n");
                }
                ParseError::BadVersion => {
                    usart1_write_bytes(b"ParseError::BadVersion\r\n");
                }
                ParseError::SegmentOutOfBounds => {
                    usart1_write_bytes(b"ParseError::SegmentOutOfBounds\r\n");
                }
                ParseError::TablesOutOfBounds => {
                    usart1_write_bytes(b"ParseError::TablesOutOfBounds\r\n");
                }
            }

            loop {
                cortex_m::asm::wfi();
            }
        }
    };

    for seg in image.segments() {
        let dest = seg.dest();

        usart1_write_bytes(TICK_ZERO);
        usart1_write_bytes(b"bootloader:   seg: ");
        usart1_write_hex(dest);
        usart1_write_bytes(b" file=");
        usart1_write_hex(seg.file_size());
        usart1_write_bytes(b" mem=");
        usart1_write_hex(seg.mem_size());
        usart1_write_bytes(b"\r\n");

        ptr::copy(
            seg.data().as_ptr(),
            dest as *mut u8,
            seg.file_size() as usize,
        );

        // Zero-fill .bss
        let bss_start = dest + seg.file_size();
        let bss_len = seg.zero_fill() as usize;
        if bss_len > 0 {
            ptr::write_bytes(bss_start as *mut u8, 0, bss_len);
        }
    }

    let entry = image.entry_point();
    usart1_write_bytes(TICK_ZERO);
    usart1_write_bytes(b"bootloader: jump ");
    usart1_write_hex(entry);
    usart1_write_bytes(b"\r\n");
    core::arch::asm!(
        "ldr sp, [{entry}]",
        "ldr pc, [{entry}, #4]",
        entry = in(reg) entry,
        options(noreturn),
    );
}

extern "C" {
    fn bootloader_must_not_panic() -> !;
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    unsafe { bootloader_must_not_panic() }
}
