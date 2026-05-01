//! Letter-rev LCPU patch installer (phase 6 of bringup).
//!
//! Two mechanisms:
//! 1. **PACH header** at `0x2040_5000` — the LCPU ROM reads it to find
//!    the patch code's entry point and Thumb bit.
//! 2. **PATCH peripheral entries** programmed from `LIST_BIN` —
//!    Cortex-M-FPB-style instruction interception that replaces ROM
//!    fetches at runtime. The list file's header is `PTCH` magic +
//!    byte-size; entries are 8 bytes each (`u32 break_addr`, `u32 data`).
//!
//! Source-of-truth: `sifli-rs/sifli-hal/src/patch.rs::install_letter()`.

use core::ptr;

use sifli_pac::{LPSYS_RCC, PATCH};

use crate::addr;

const LETTER_PATCH_BUF_MAGIC: u32 = 0x4843_4150; // "PACH"
const LETTER_ENTRY_COUNT: u32 = 7;

const LIST_FILE_MAGIC: u32 = 0x5054_4348; // "PTCH"

/// Maximum hardware patch channels.
const MAX_PATCH_ENTRIES: usize = 32;
/// Address mask for the channel ADDR field (bits 18:2).
const PATCH_ADDR_MASK: u32 = 0x0007_FFFC;

/// Patch entry table. Header (8 B) + N × {u32 break_addr, u32 data}.
const LIST_BIN: &[u8] = include_bytes!("../data/patch_letter_list.bin");
/// Patch code blob copied to `PATCH_CODE_START`.
const PATCH_BIN: &[u8] = include_bytes!("../data/patch_letter_bin.bin");

#[derive(Debug, Copy, Clone, PartialEq, Eq, rcard_log::Format)]
pub enum Error {
    /// Patch code is larger than the reserved code region.
    CodeTooLarge,
    /// Patch list file's `PTCH` magic was wrong.
    BadListMagic,
    /// Too many entries to fit in the 32 hardware channels.
    TooManyEntries,
}

/// Install the Letter-rev patches. Must be called with LCPU held in
/// reset (CPUWAIT high) so the LCPU ROM doesn't observe a half-written
/// patch header / code region.
pub fn install_letter() -> Result<(), Error> {
    if PATCH_BIN.len() > addr::PATCH_CODE_SIZE {
        return Err(Error::CodeTooLarge);
    }

    // Step 0 — clear the first 0x500 bytes of LCPU SRAM.
    // Matches sifli-rs's `RamSlice::new(LPSYS_RAM_BASE, 0x500).clear()`,
    // which the SDK does via `memset((void *)0x20400000, 0, 0x500)`.
    unsafe {
        ptr::write_bytes(addr::LPSYS_RAM_BASE as *mut u8, 0, 0x500);
    }

    // Step 1 — write PACH header (magic, fixed entry count, code entry
    // point with Thumb bit) at 0x2040_5000. The ROM reads these three
    // u32s before jumping into patched code.
    unsafe {
        let buf = addr::PATCH_BUF_START as *mut u32;
        ptr::write_volatile(buf, LETTER_PATCH_BUF_MAGIC);
        ptr::write_volatile(buf.add(1), LETTER_ENTRY_COUNT);
        ptr::write_volatile(buf.add(2), addr::PATCH_CODE_START_LCPU | 0x1);
    }

    // Step 2 — clear + copy patch code blob to 0x2040_500C.
    unsafe {
        ptr::write_bytes(
            addr::PATCH_CODE_START as *mut u8,
            0,
            addr::PATCH_CODE_SIZE,
        );
        ptr::copy_nonoverlapping(
            PATCH_BIN.as_ptr(),
            addr::PATCH_CODE_START as *mut u8,
            PATCH_BIN.len(),
        );
    }

    // Step 3 — drive PATCH peripheral channels from LIST_BIN.
    install_hw_entries(LIST_BIN)?;

    Ok(())
}

/// Walk the patch-list blob (PTCH header + 8-byte entries) and program
/// the PATCH peripheral channels.
fn install_hw_entries(list: &[u8]) -> Result<(), Error> {
    if list.len() < 8 {
        return Err(Error::BadListMagic);
    }
    let magic = unsafe { ptr::read_unaligned(list.as_ptr() as *const u32) };
    if magic != LIST_FILE_MAGIC {
        return Err(Error::BadListMagic);
    }
    let size_bytes = unsafe { ptr::read_unaligned(list.as_ptr().add(4) as *const u32) } as usize;
    let entry_count = size_bytes / 8;
    if entry_count > MAX_PATCH_ENTRIES {
        return Err(Error::TooManyEntries);
    }
    if 8 + size_bytes > list.len() {
        return Err(Error::BadListMagic);
    }

    // Enable PATCH peripheral clock — disabled after LCPU reset, AHB
    // accesses to PATCH stall otherwise.
    LPSYS_RCC.esr1().write(|w| w.set_patch(true));

    // Disable all channels first so partial state from a prior run
    // doesn't apply during reprogramming.
    PATCH.cer().write(|w| w.set_ce(0));

    let mut enabled_mask: u32 = 0;
    let entries = &list[8..];
    for i in 0..entry_count {
        let off = i * 8;
        let break_addr =
            unsafe { ptr::read_unaligned(entries.as_ptr().add(off) as *const u32) };
        let data =
            unsafe { ptr::read_unaligned(entries.as_ptr().add(off + 4) as *const u32) };

        let addr_masked = break_addr & PATCH_ADDR_MASK;
        PATCH.ch(i).write(|w| w.set_addr(addr_masked >> 2));
        PATCH.csr().write(|w| w.set_cs(1u32 << i));
        PATCH.cdr().write(|w| w.set_data(data));
        enabled_mask |= 1u32 << i;
    }

    // Clear channel-select latch.
    PATCH.csr().write(|w| w.set_cs(0));
    // Enable all programmed channels in one go.
    PATCH.cer().write(|w| w.set_ce(enabled_mask));

    Ok(())
}
