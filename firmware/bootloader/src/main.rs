#![no_std]
#![no_main]

use core::ptr;

use rcard_places::{FirmwareState, ParseError, PlacesImage, SelectionReason, Slot};

const FTAB_ADDR: u32 = 0x1200_0000;

const FTAB_SLOT_A: usize = 14;
const FTAB_SLOT_B: usize = 15;

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

fn write_firmware_state(state: FirmwareState) {
    match state {
        FirmwareState::Default => usart1_write_bytes(b"Default"),
        FirmwareState::IntegrityCompromised => {
            usart1_write_bytes(b"IntegrityCompromised");
        }
        FirmwareState::WatchdogConcern => usart1_write_bytes(b"WatchdogConcern"),
        FirmwareState::KernelConcern => usart1_write_bytes(b"KernelConcern"),
        FirmwareState::SupervisorConcern => usart1_write_bytes(b"SupervisorConcern"),
        FirmwareState::RuntimeConcern => usart1_write_bytes(b"RuntimeConcern"),
    }
}

fn write_selection_reason(reason: SelectionReason) {
    match reason {
        SelectionReason::OtherConcerned(state) => {
            usart1_write_bytes(b"other slot: ");
            write_firmware_state(state);
        }
        SelectionReason::HigherVersion => {
            usart1_write_bytes(b"higher version");
        }
        SelectionReason::NewerFlash => {
            usart1_write_bytes(b"newer flash timestamp");
        }
        SelectionReason::Tiebreak => {
            usart1_write_bytes(b"tiebreak");
        }
    }
}

/// Try to parse a places image from an ftab slot. Returns None if the
/// slot is empty (erased flash) or the image is unparseable.
unsafe fn try_parse_slot(slot: usize) -> Option<(&'static [u8], PlacesImage<'static>)> {
    let (base, size) = read_ftab_slot(slot);
    // Erased flash: base and size are both 0xFFFFFFFF.
    if base == 0xFFFF_FFFF || size == 0 || size == 0xFFFF_FFFF {
        return None;
    }
    let data = core::slice::from_raw_parts(base as *const u8, size as usize);
    PlacesImage::parse(data).ok().map(|img| (data, img))
}

#[no_mangle]
pub unsafe extern "C" fn bootloader_main() -> ! {
    usart1_write_bytes(TICK_ZERO);
    usart1_write_bytes(b"bootloader: awake\r\n");

    // ── A/B firmware selection ────────────────────────────────────
    let slot_a = try_parse_slot(FTAB_SLOT_A);
    let slot_b = try_parse_slot(FTAB_SLOT_B);

    let selected = match (slot_a, slot_b) {
        (Some((_, ref a)), Some((_, ref b))) => {
            match rcard_places::select_firmware(a, b) {
                Ok((slot, reason)) => {
                    usart1_write_bytes(TICK_ZERO);
                    usart1_write_bytes(match slot {
                        Slot::A => b"bootloader: selected slot A (",
                        Slot::B => b"bootloader: selected slot B (",
                    });
                    write_selection_reason(reason);
                    usart1_write_bytes(b")\r\n");
                    slot
                }
                Err(rcard_places::SelectionError::BothConcerned { a, b }) => {
                    usart1_write_bytes(TICK_ZERO);
                    usart1_write_bytes(b"bootloader: FATAL both slots concerned\r\n");
                    usart1_write_bytes(TICK_ZERO);
                    usart1_write_bytes(b"  slot A: ");
                    write_firmware_state(a);
                    usart1_write_bytes(b"\r\n");
                    usart1_write_bytes(TICK_ZERO);
                    usart1_write_bytes(b"  slot B: ");
                    write_firmware_state(b);
                    usart1_write_bytes(b"\r\n");
                    loop { cortex_m::asm::wfi(); }
                }
            }
        }
        (Some(_), None) => {
            usart1_write_bytes(TICK_ZERO);
            usart1_write_bytes(b"bootloader: slot B empty, using A\r\n");
            Slot::A
        }
        (None, Some(_)) => {
            usart1_write_bytes(TICK_ZERO);
            usart1_write_bytes(b"bootloader: slot A empty, using B\r\n");
            Slot::B
        }
        (None, None) => {
            usart1_write_bytes(TICK_ZERO);
            usart1_write_bytes(b"bootloader: FATAL no valid firmware\r\n");
            loop { cortex_m::asm::wfi(); }
        }
    };

    // Re-parse the selected slot (we need the image for segment iteration).
    let selected_slot = if selected == Slot::A { FTAB_SLOT_A } else { FTAB_SLOT_B };
    let (places_base, places_size) = read_ftab_slot(selected_slot);
    let places_data = core::slice::from_raw_parts(places_base as *const u8, places_size as usize);
    let image = match PlacesImage::parse(places_data) {
        Ok(img) => img,
        Err(e) => {
            usart1_write_bytes(TICK_ZERO);
            usart1_write_bytes(b"bootloader: bad places.bin: ");
            match e {
                ParseError::TooSmall => usart1_write_bytes(b"TooSmall\r\n"),
                ParseError::BadMagic => usart1_write_bytes(b"BadMagic\r\n"),
                ParseError::BadVersion => usart1_write_bytes(b"BadVersion\r\n"),
                ParseError::SegmentOutOfBounds => usart1_write_bytes(b"SegmentOOB\r\n"),
                ParseError::TablesOutOfBounds => usart1_write_bytes(b"TablesOOB\r\n"),
            }
            loop { cortex_m::asm::wfi(); }
        }
    };

    // ── Load segments ────────────────────────────────────────────
    for seg in image.segments() {
        let dest = seg.dest();

        usart1_write_bytes(TICK_ZERO);
        usart1_write_bytes(b"bootloader:   seg: ");
        usart1_write_hex(dest);
        usart1_write_bytes(b" file=");
        usart1_write_hex(seg.file_size());
        usart1_write_bytes(b" mem=");
        usart1_write_hex(seg.mem_size());

        // Skip copy and .bss zero-fill for flash-resident XIP segments
        // (dest already points at the data within the places image).
        let src = seg.data().as_ptr() as u32;
        usart1_write_bytes(b" src=");
        usart1_write_hex(src);
        let self_resident = dest == src;
        if self_resident {
            usart1_write_bytes(b"  (xip, skip)");
        } else {
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

        usart1_write_bytes(b"\r\n");
    }

    // Enable I-cache and D-cache. The bootrom leaves both disabled
    // (mpu_config/cache_enable are stubbed as no-ops in bf0_ap_hal_msp.c).
    // Without caches, every instruction fetch from MPI2 XIP goes over SPI.
    const SCB_CCR: *mut u32 = 0xE000_ED14 as *mut u32;
    const SCB_ICIALLU: *mut u32 = 0xE000_EF50 as *mut u32;
    const SCB_DCISW: *mut u32 = 0xE000_EF60 as *mut u32;
    const SCB_CCSIDR: *const u32 = 0xE000_ED80 as *const u32;
    const SCB_CSSELR: *mut u32 = 0xE000_ED84 as *mut u32;
    const CCR_IC: u32 = 1 << 17;
    const CCR_DC: u32 = 1 << 16;

    // I-cache: invalidate all, then enable
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    ptr::write_volatile(SCB_ICIALLU, 0);
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
    let ccr = ptr::read_volatile(SCB_CCR);
    ptr::write_volatile(SCB_CCR, ccr | CCR_IC);
    cortex_m::asm::dsb();
    cortex_m::asm::isb();

    // Enable D-cache now that all RAM copies are done. Invalidate all
    // sets/ways first so the cache is clean.
    ptr::write_volatile(SCB_CSSELR, 0); // select L1 data cache
    cortex_m::asm::dsb();
    let ccsidr = ptr::read_volatile(SCB_CCSIDR);
    let sets = (ccsidr >> 13) & 0x7FFF;
    let ways = (ccsidr >> 3) & 0x3FF;
    for way in 0..=ways {
        for set in 0..=sets {
            ptr::write_volatile(SCB_DCISW, (way << 30) | (set << 5));
        }
    }
    cortex_m::asm::dsb();
    let ccr = ptr::read_volatile(SCB_CCR);
    ptr::write_volatile(SCB_CCR, ccr | CCR_DC);
    cortex_m::asm::dsb();
    cortex_m::asm::isb();

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
