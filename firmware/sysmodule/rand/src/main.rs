#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use sysmodule_rand_api::*;

const TRNG_BASE: usize = 0x5000_F000;

const CTRL: *mut u32 = TRNG_BASE as *mut u32; // +0x00
const STAT: *const u32 = (TRNG_BASE + 0x04) as *const u32; // +0x04
const RAND_NUM0: *const u32 = (TRNG_BASE + 0x30) as *const u32;

const CTRL_GEN_SEED_START: u32 = 1 << 0;
const CTRL_GEN_RAND_NUM_START: u32 = 1 << 1;

const STAT_SEED_VALID: u32 = 1 << 1;
const STAT_RAND_NUM_VALID: u32 = 1 << 3;

const POLL_LIMIT: u32 = 100_000;

fn poll_stat(mask: u32) -> bool {
    let mut polls = 0u32;
    while unsafe { read_volatile(STAT) } & mask == 0 {
        polls += 1;
        if polls >= POLL_LIMIT {
            return false;
        }
    }
    true
}

struct RandImpl;

impl Rand for RandImpl {
    fn generate(_meta: ipc::Meta) -> Result<[u8; 32], RandError> {
        unsafe {
            write_volatile(CTRL, CTRL_GEN_SEED_START);
        }
        if !poll_stat(STAT_SEED_VALID) {
            return Err(RandError::SeedTimeout);
        }

        unsafe {
            write_volatile(CTRL, CTRL_GEN_RAND_NUM_START);
        }
        if !poll_stat(STAT_RAND_NUM_VALID) {
            return Err(RandError::GenerateTimeout);
        }

        let mut out = [0u8; 32];
        let mut i = 0;
        while i < 8 {
            let word = unsafe { read_volatile(RAND_NUM0.add(i)) };
            let bytes = word.to_le_bytes();
            let base = i * 4;
            out[base] = bytes[0];
            out[base + 1] = bytes[1];
            out[base + 2] = bytes[2];
            out[base + 3] = bytes[3];
            i += 1;
        }
        Ok(out)
    }
}

fn enable_trng_clock() {
    let rcc = sifli_pac::HPSYS_RCC;
    rcc.enr1().modify(|w| w.set_trng(true));
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    userlib::sys_panic(b"rand panic")
}

#[export_name = "main"]
fn main() -> ! {
    enable_trng_clock();

    ipc::server! {
        Rand: RandImpl,
    }
}
