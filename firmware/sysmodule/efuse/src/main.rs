#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use sysmodule_efuse_api::*;

// ---------------------------------------------------------------------------
// efusec register map (SF32LB52, §13.3.4)
// ---------------------------------------------------------------------------

const EFUSEC_BASE: usize = 0x5000_C000;

const CR: *mut u32 = EFUSEC_BASE as *mut u32; // +0x00
const SR: *mut u32 = (EFUSEC_BASE + 0x08) as *mut u32; // +0x08
const BANK_DATA: *const u32 = (EFUSEC_BASE + 0x30) as *const u32; // bank0 word0

// PMUC.HPSYS_VOUT (0x500CA000 + 0x94): bits [3:0] = vout.
// The eFuse array requires an elevated supply for reliable read margin.
// Boost vout by +3 (clamped to 0xF) before reading and restore after,
// matching the Zephyr otp_sifli_efuse driver sequence.
const PMUC_HPSYS_VOUT: *mut u32 = 0x500C_A094 as *mut u32;

const CR_EN: u32 = 1 << 0;
// CR.MODE is bit 1, 0 = READ; we always issue READs so we leave it 0.
const CR_BANKSEL_SHIFT: u32 = 2;

const SR_DONE: u32 = 1 << 0;

/// Maximum poll iterations before declaring a timeout. At 240 MHz the
/// real controller completes in well under 1 µs; 100 000 iterations is
/// extremely generous while still preventing an infinite loop when the
/// peripheral is unmodeled (e.g. Renode with a SilenceRange).
const POLL_LIMIT: u32 = 100_000;

/// Run one bank read: program CR, poll SR.DONE, w1c DONE, then pull the
/// eight data words out. See §13.3.4 for the sequence.
fn read_bank(bank: u8) -> Result<[u32; 8], EfuseError> {
    let bank = bank as u32 & 0x3;

    unsafe {
        // Boost HPSYS_VOUT by +3 (clamped to 0xF) for read margin, then
        // wait ~20 µs for the LDO to settle before initiating the read.
        let orig_vout = read_volatile(PMUC_HPSYS_VOUT);
        let boosted_vout = (orig_vout & !0xF) | (((orig_vout & 0xF) + 3).min(0xF));
        write_volatile(PMUC_HPSYS_VOUT, boosted_vout);
        for _ in 0..10_000u32 {
            core::arch::asm!("nop");
        }

        // BANKSEL = bank, MODE = READ (0), EN = 1 (self-clearing, kicks
        // off the read).
        write_volatile(CR, (bank << CR_BANKSEL_SHIFT) | CR_EN);

        // Poll SR.DONE with a bounded retry count.
        let mut polls = 0u32;
        while read_volatile(SR) & SR_DONE == 0 {
            polls += 1;
            if polls >= POLL_LIMIT {
                write_volatile(PMUC_HPSYS_VOUT, orig_vout);
                return Err(EfuseError::Timeout);
            }
        }

        // w1c the DONE flag before the next read.
        write_volatile(SR, SR_DONE);

        // Each bank is 8 × 32-bit words at BANK{n}_DATA0..7.
        let base = BANK_DATA.add((bank as usize) * 8);
        let mut out = [0u32; 8];
        let mut i = 0;
        while i < 8 {
            out[i] = read_volatile(base.add(i));
            i += 1;
        }

        write_volatile(PMUC_HPSYS_VOUT, orig_vout);
        Ok(out)
    }
}

/// Serialize the 8 bank words into a 32-byte little-endian buffer.
fn bank_to_bytes(words: [u32; 8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 8 {
        let bytes = words[i].to_le_bytes();
        let base = i * 4;
        out[base] = bytes[0];
        out[base + 1] = bytes[1];
        out[base + 2] = bytes[2];
        out[base + 3] = bytes[3];
        i += 1;
    }
    out
}

struct EfuseImpl;

impl Efuse for EfuseImpl {
    fn read(_meta: ipc::Meta, bank_id: u8) -> Result<[u8; 32], EfuseError> {
        if bank_id > 3 {
            return Err(EfuseError::InvalidBank);
        }
        Ok(bank_to_bytes(read_bank(bank_id)?))
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    userlib::sys_panic(b"efuse panic")
}

#[export_name = "main"]
fn main() -> ! {
    ipc::server! {
        Efuse: EfuseImpl,
    }
}
