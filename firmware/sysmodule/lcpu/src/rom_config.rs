//! Letter-rev LCPU ROM configuration block writer (phase 3 of bringup).
//!
//! Layout source: sifli-rs `sifli-radio/data/sf32lb52x/rom_config_layout.toml`.
//! Field offsets are relative to `addr::ROM_CONFIG_BASE_LETTER`.

use core::ptr;

use crate::addr;

/// Magic at offset +0 (`uint32_t magic = 0x4545_7878`).
pub const MAGIC_VALUE: u32 = 0x4545_7878;

// Field offsets (Letter rev) — relative to ROM_CONFIG_BASE_LETTER.
const OFF_MAGIC: usize = 0;
const OFF_WDT_TIME: usize = 12;
const OFF_WDT_STATUS: usize = 16;
const OFF_WDT_CLK: usize = 24;
const OFF_IS_XTAL_ENABLE: usize = 26;
const OFF_IS_RCCAL_IN_L: usize = 27;
const OFF_BT_ROM_CONFIG: usize = 172;
const OFF_HCPU_IPC_ADDR: usize = 200;

// BtRomConfig sub-field offsets (relative to OFF_BT_ROM_CONFIG).
const BT_OFF_BIT_VALID: usize = 0;
const BT_OFF_CONTROLLER_ENABLE_BIT: usize = 8;
const BT_OFF_LLD_PROG_DELAY: usize = 9;
const BT_OFF_DEFAULT_SLEEP_MODE: usize = 11;
const BT_OFF_DEFAULT_SLEEP_ENABLED: usize = 12;
const BT_OFF_DEFAULT_XTAL_ENABLED: usize = 13;
const BT_OFF_DEFAULT_RC_CYCLE: usize = 14;
const BT_OFF_IS_FPGA: usize = 17;

// BtRomConfig.bit_valid bits (positions in the u32 mask).
const VALID_CONTROLLER_ENABLE_BIT: u32 = 1 << 1;
const VALID_LLD_PROG_DELAY: u32 = 1 << 2;
const VALID_DEFAULT_SLEEP_MODE: u32 = 1 << 4;
const VALID_DEFAULT_SLEEP_ENABLED: u32 = 1 << 5;
const VALID_DEFAULT_XTAL_ENABLED: u32 = 1 << 6;
const VALID_DEFAULT_RC_CYCLE: u32 = 1 << 7;
const VALID_IS_FPGA: u32 = 1 << 10;

/// Static defaults matching sifli-rs's `RomConfig::default()` and
/// `ControllerConfig::default()`. `pm_enabled = false` keeps the controller
/// out of the LP-sleep code path on first cut (no idle-hook patching).
const WDT_TIME: u32 = 10;
const WDT_STATUS: u32 = 0xFF;
const WDT_CLK: u16 = 32_768;
const ENABLE_LXT: bool = true;
const LLD_PROG_DELAY: u8 = 3;
const DEFAULT_RC_CYCLE: u8 = 20;

/// Write the Letter-rev ROM-config block. `hcpu_ipc_addr` is the HCPU
/// address of the qid-0 TX ring (`HCPU_IPC_ADDR` field at +200) — LCPU
/// reads this to find the mailbox.
pub fn write_letter(hcpu_ipc_addr: u32) {
    // Clear the entire block first so any prior bring-up leaves no
    // residual fields half-set.
    unsafe {
        ptr::write_bytes(
            addr::ROM_CONFIG_BASE_LETTER as *mut u8,
            0,
            addr::ROM_CONFIG_SIZE_LETTER,
        );
    }

    write_u32(OFF_MAGIC, MAGIC_VALUE);
    write_u32(OFF_WDT_TIME, WDT_TIME);
    write_u32(OFF_WDT_STATUS, WDT_STATUS);
    write_u16(OFF_WDT_CLK, WDT_CLK);
    write_u8(OFF_IS_XTAL_ENABLE, ENABLE_LXT as u8);
    write_u8(OFF_IS_RCCAL_IN_L, (!ENABLE_LXT) as u8);

    // BtRomConfig sub-struct.
    let bt = OFF_BT_ROM_CONFIG;
    let bit_valid = VALID_CONTROLLER_ENABLE_BIT
        | VALID_LLD_PROG_DELAY
        | VALID_DEFAULT_SLEEP_MODE
        | VALID_DEFAULT_SLEEP_ENABLED
        | VALID_DEFAULT_XTAL_ENABLED
        | VALID_DEFAULT_RC_CYCLE
        | VALID_IS_FPGA;
    write_u32(bt + BT_OFF_BIT_VALID, bit_valid);
    write_u8(bt + BT_OFF_CONTROLLER_ENABLE_BIT, 0x03); // BLE + BT both on
    write_u8(bt + BT_OFF_LLD_PROG_DELAY, LLD_PROG_DELAY);
    write_u8(bt + BT_OFF_DEFAULT_SLEEP_MODE, 0);
    write_u8(bt + BT_OFF_DEFAULT_SLEEP_ENABLED, 0); // pm_enabled=false
    write_u8(bt + BT_OFF_DEFAULT_XTAL_ENABLED, 1); // xtal_enabled=true
    write_u8(bt + BT_OFF_DEFAULT_RC_CYCLE, DEFAULT_RC_CYCLE);
    write_u8(bt + BT_OFF_IS_FPGA, 0);

    write_u32(OFF_HCPU_IPC_ADDR, hcpu_ipc_addr);
}

#[inline]
fn write_u32(offset: usize, val: u32) {
    let p = (addr::ROM_CONFIG_BASE_LETTER + offset) as *mut u32;
    unsafe { ptr::write_volatile(p, val) }
}

#[inline]
fn write_u16(offset: usize, val: u16) {
    let p = (addr::ROM_CONFIG_BASE_LETTER + offset) as *mut u16;
    unsafe { ptr::write_volatile(p, val) }
}

#[inline]
fn write_u8(offset: usize, val: u8) {
    let p = (addr::ROM_CONFIG_BASE_LETTER + offset) as *mut u8;
    unsafe { ptr::write_volatile(p, val) }
}
