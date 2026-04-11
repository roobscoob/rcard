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

const CR_EN: u32 = 1 << 0;
// CR.MODE is bit 1, 0 = READ; we always issue READs so we leave it 0.
const CR_BANKSEL_SHIFT: u32 = 2;

const SR_DONE: u32 = 1 << 0;

/// Run one bank read: program CR, poll SR.DONE, w1c DONE, then pull the
/// eight data words out. See §13.3.4 for the sequence.
fn read_bank(bank: u8) -> [u32; 8] {
    let bank = bank as u32 & 0x3;

    unsafe {
        // BANKSEL = bank, MODE = READ (0), EN = 1 (self-clearing, kicks
        // off the read).
        write_volatile(CR, (bank << CR_BANKSEL_SHIFT) | CR_EN);

        // Poll SR.DONE. The TIMR reset value is calibrated for a 48 MHz
        // module clock; the completion window is sub-millisecond so a
        // tight busy-poll is fine.
        while read_volatile(SR) & SR_DONE == 0 {}

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
        out
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
        Ok(bank_to_bytes(read_bank(bank_id)))
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
