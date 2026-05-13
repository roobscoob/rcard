//! LCPU patch installer (phase 6 of bringup).
//!
//! Two mechanisms shared by both revs:
//! 1. **Patch header in RAM**: tells the LCPU ROM where to find the
//!    patched code's entry point.
//!    - Letter: `PACH` magic (`0x4843_4150`) + fixed entry count + Thumb
//!      entry address at `PATCH_BUF_START_LETTER` (`0x2040_5000`).
//!    - A3: the patch list blob itself (with `PTCH` magic, copied into
//!      RAM at `PATCH_RECORD_ADDR_A3`) doubles as the header.
//! 2. **PATCH peripheral entries** programmed from the list blob —
//!    Cortex-M-FPB-style instruction interception that replaces ROM
//!    fetches at runtime. List file header: `PTCH` magic + byte-size;
//!    entries are 8 bytes each (`u32 break_addr`, `u32 data`).
//!
//! Source: `sifli-rs/sifli-hal/src/patch.rs::{install_letter, install_a3}`.

use core::ptr;

use sifli_pac::{LPSYS_RCC, PATCH};
use sysmodule_syscon_api::ChipRev;

use crate::addr;

const LETTER_PATCH_BUF_MAGIC: u32 = 0x4843_4150; // "PACH"
const LETTER_ENTRY_COUNT: u32 = 7;

const LIST_FILE_MAGIC: u32 = 0x5054_4348; // "PTCH"

/// Maximum hardware patch channels.
const MAX_PATCH_ENTRIES: usize = 32;
/// Address mask for the channel ADDR field (bits 18:2).
const PATCH_ADDR_MASK: u32 = 0x0007_FFFC;

/// Letter patch list (header + 8-byte entries, embedded in flash).
const LIST_BIN_LETTER: &[u8] = include_bytes!("../data/patch_letter_list.bin");
/// Letter patch code blob copied to `PATCH_CODE_START_LETTER`.
const PATCH_BIN_LETTER: &[u8] = include_bytes!("../data/patch_letter_bin.bin");

/// A3 patch list (header + 8-byte entries, embedded in flash; later
/// memcpy'd to `PATCH_RECORD_ADDR_A3` so PATCH peripheral can read it).
const LIST_BIN_A3: &[u8] = include_bytes!("../data/patch_a3_list.bin");
/// A3 patch code blob copied to `PATCH_CODE_START_A3`.
const PATCH_BIN_A3: &[u8] = include_bytes!("../data/patch_a3_bin.bin");

#[derive(Debug, Copy, Clone, PartialEq, Eq, rcard_log::Format)]
pub enum Error {
    /// Patch code is larger than the reserved code region.
    CodeTooLarge,
    /// Patch list blob's `PTCH` magic was wrong.
    BadListMagic,
    /// Patch list blob is too large to fit in the A3 record region.
    ListTooLarge,
    /// Too many entries to fit in the 32 hardware channels.
    TooManyEntries,
}

/// Dispatch to the rev-specific installer.
pub fn install(rev: ChipRev) -> Result<(), Error> {
    match rev {
        ChipRev::Letter => install_letter(),
        ChipRev::A3OrEarlier => install_a3(),
    }
}

/// Install the Letter-rev patches. Must be called with LCPU held in
/// reset (CPUWAIT high) so the LCPU ROM doesn't observe a half-written
/// patch header / code region.
fn install_letter() -> Result<(), Error> {
    if PATCH_BIN_LETTER.len() > addr::PATCH_CODE_SIZE_LETTER {
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
        let buf = addr::PATCH_BUF_START_LETTER as *mut u32;
        ptr::write_volatile(buf, LETTER_PATCH_BUF_MAGIC);
        ptr::write_volatile(buf.add(1), LETTER_ENTRY_COUNT);
        ptr::write_volatile(buf.add(2), addr::PATCH_CODE_START_LCPU_LETTER | 0x1);
    }

    // Step 2 — clear + copy patch code blob to 0x2040_500C.
    unsafe {
        ptr::write_bytes(
            addr::PATCH_CODE_START_LETTER as *mut u8,
            0,
            addr::PATCH_CODE_SIZE_LETTER,
        );
        ptr::copy_nonoverlapping(
            PATCH_BIN_LETTER.as_ptr(),
            addr::PATCH_CODE_START_LETTER as *mut u8,
            PATCH_BIN_LETTER.len(),
        );
    }

    // Step 3 — drive PATCH peripheral channels from the in-flash blob.
    install_hw_entries_from_slice(LIST_BIN_LETTER)?;

    Ok(())
}

/// Install the A3-rev patches. The A3 PATCH peripheral reads its record
/// from RAM (not flash), so we must memcpy the list blob into LCPU SRAM
/// first. Letter doesn't need this — the entries are read straight from
/// the embedded slice.
fn install_a3() -> Result<(), Error> {
    if PATCH_BIN_A3.len() > addr::PATCH_CODE_SIZE_A3 {
        return Err(Error::CodeTooLarge);
    }
    if LIST_BIN_A3.len() > addr::PATCH_RECORD_SIZE_A3 {
        return Err(Error::ListTooLarge);
    }

    // Step 1 — copy patch list blob to RAM. Per sifli-rs's
    // `install_a3`, we do **not** clear LPSYS_RAM_BASE first — the
    // firmware blob loaded in phase 5 lives there and must survive.
    unsafe {
        ptr::copy_nonoverlapping(
            LIST_BIN_A3.as_ptr(),
            addr::PATCH_RECORD_ADDR_A3 as *mut u8,
            LIST_BIN_A3.len(),
        );
    }

    // Step 2 — drive PATCH peripheral channels reading from the
    // in-RAM record we just wrote.
    install_hw_entries_from_addr(addr::PATCH_RECORD_ADDR_A3, LIST_BIN_A3.len())?;

    // Step 3 — clear and copy patch code blob to PATCH_CODE_START_A3.
    // SDK does this *after* configuring PATCH HW.
    unsafe {
        ptr::write_bytes(
            addr::PATCH_CODE_START_A3 as *mut u8,
            0,
            addr::PATCH_CODE_SIZE_A3,
        );
        ptr::copy_nonoverlapping(
            PATCH_BIN_A3.as_ptr(),
            addr::PATCH_CODE_START_A3 as *mut u8,
            PATCH_BIN_A3.len(),
        );
    }

    Ok(())
}

/// Letter path: drive PATCH peripheral entries from an in-flash blob.
fn install_hw_entries_from_slice(list: &[u8]) -> Result<(), Error> {
    if list.len() < 8 {
        return Err(Error::BadListMagic);
    }
    install_hw_entries_from_addr(list.as_ptr() as usize, list.len())
}

/// Common path: walk PTCH-header + 8-byte entries at `record_addr`
/// (which may be in flash or in LCPU SRAM) and program the PATCH
/// peripheral channels.
fn install_hw_entries_from_addr(record_addr: usize, len: usize) -> Result<(), Error> {
    if len < 8 {
        return Err(Error::BadListMagic);
    }
    let magic = unsafe { ptr::read_unaligned(record_addr as *const u32) };
    if magic != LIST_FILE_MAGIC {
        return Err(Error::BadListMagic);
    }
    let size_bytes =
        unsafe { ptr::read_unaligned((record_addr + 4) as *const u32) } as usize;
    let entry_count = size_bytes / 8;
    if entry_count > MAX_PATCH_ENTRIES {
        return Err(Error::TooManyEntries);
    }
    if 8 + size_bytes > len {
        return Err(Error::BadListMagic);
    }

    // Enable PATCH peripheral clock — disabled after LCPU reset, AHB
    // accesses to PATCH stall otherwise.
    LPSYS_RCC.esr1().write(|w| w.set_patch(true));

    // Disable all channels first so partial state from a prior run
    // doesn't apply during reprogramming.
    PATCH.cer().write(|w| w.set_ce(0));

    let mut enabled_mask: u32 = 0;
    let entries_addr = record_addr + 8;
    for i in 0..entry_count {
        let off = i * 8;
        let break_addr =
            unsafe { ptr::read_unaligned((entries_addr + off) as *const u32) };
        let data =
            unsafe { ptr::read_unaligned((entries_addr + off + 4) as *const u32) };

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
