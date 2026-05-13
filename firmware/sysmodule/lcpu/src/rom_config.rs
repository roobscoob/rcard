//! LCPU ROM configuration block writer (phase 3 of bringup).
//!
//! Letter rev writes the full 204-byte block at `ROM_CONFIG_BASE_LETTER`
//! including `BtRomConfig` and `HCPU_IPC_ADDR`.
//!
//! A3 rev only writes magic + watchdog fields at `ROM_CONFIG_BASE_A3`
//! (64 B used). A3's `BtRomConfig` lives at `G_ROM_CONFIG_A3` and is
//! written **post-boot** in `controller::post_init_a3`.
//!
//! Layout source: sifli-rs `sifli-radio/data/sf32lb52x/rom_config_layout.toml`.

use core::ptr;

use sysmodule_syscon_api::ChipRev;

use crate::addr;

/// Magic at offset +0 (`uint32_t magic = 0x4545_7878`).
pub const MAGIC_VALUE: u32 = 0x4545_7878;

// Common (A3 + Letter) field offsets relative to the ROM-config base.
const OFF_MAGIC: usize = 0;
const OFF_WDT_TIME: usize = 12;
const OFF_WDT_STATUS: usize = 16;
const OFF_WDT_CLK: usize = 24;
const OFF_IS_XTAL_ENABLE: usize = 26;
const OFF_IS_RCCAL_IN_L: usize = 27;

// Letter-only field offsets.
const OFF_BT_ROM_CONFIG: usize = 172;
const OFF_HCPU_IPC_ADDR: usize = 200;

// BtRomConfig sub-field offsets (relative to OFF_BT_ROM_CONFIG on Letter
// or to G_ROM_CONFIG_A3 on A3 — same layout in both cases).
pub(crate) const BT_OFF_BIT_VALID: usize = 0;
pub(crate) const BT_OFF_CONTROLLER_ENABLE_BIT: usize = 8;
pub(crate) const BT_OFF_LLD_PROG_DELAY: usize = 9;
pub(crate) const BT_OFF_DEFAULT_SLEEP_MODE: usize = 11;
pub(crate) const BT_OFF_DEFAULT_SLEEP_ENABLED: usize = 12;
pub(crate) const BT_OFF_DEFAULT_XTAL_ENABLED: usize = 13;
pub(crate) const BT_OFF_DEFAULT_RC_CYCLE: usize = 14;
pub(crate) const BT_OFF_IS_FPGA: usize = 17;

// BtRomConfig.bit_valid bits.
pub(crate) const VALID_CONTROLLER_ENABLE_BIT: u32 = 1 << 1;
pub(crate) const VALID_LLD_PROG_DELAY: u32 = 1 << 2;
pub(crate) const VALID_DEFAULT_SLEEP_MODE: u32 = 1 << 4;
pub(crate) const VALID_DEFAULT_SLEEP_ENABLED: u32 = 1 << 5;
pub(crate) const VALID_DEFAULT_XTAL_ENABLED: u32 = 1 << 6;
pub(crate) const VALID_DEFAULT_RC_CYCLE: u32 = 1 << 7;
pub(crate) const VALID_IS_FPGA: u32 = 1 << 10;

/// Static defaults matching sifli-rs's `RomConfig::default()` and
/// `ControllerConfig::default()`. `pm_enabled = false` keeps the
/// controller out of the LP-sleep code path on first cut (no idle-hook
/// patching).
const WDT_TIME: u32 = 10;
const WDT_STATUS: u32 = 0xFF;
const WDT_CLK: u16 = 32_768;
const ENABLE_LXT: bool = true;
pub(crate) const LLD_PROG_DELAY: u8 = 3;
pub(crate) const DEFAULT_RC_CYCLE: u8 = 20;

/// Dispatch to the rev-specific writer.
pub fn write(rev: ChipRev, hcpu_ipc_addr: u32) {
    match rev {
        ChipRev::Letter => write_letter(hcpu_ipc_addr),
        ChipRev::A3OrEarlier => write_a3(),
    }
}

/// Write the Letter-rev ROM-config block. `hcpu_ipc_addr` is the HCPU
/// address of the qid-0 TX ring (`HCPU_IPC_ADDR` field at +200) — LCPU
/// reads this to find the mailbox.
fn write_letter(hcpu_ipc_addr: u32) {
    let base = addr::ROM_CONFIG_BASE_LETTER;

    // Clear the entire block first so any prior bring-up leaves no
    // residual fields half-set.
    unsafe {
        ptr::write_bytes(base as *mut u8, 0, addr::ROM_CONFIG_SIZE_LETTER);
    }

    write_u32(base, OFF_MAGIC, MAGIC_VALUE);
    write_u32(base, OFF_WDT_TIME, WDT_TIME);
    write_u32(base, OFF_WDT_STATUS, WDT_STATUS);
    write_u16(base, OFF_WDT_CLK, WDT_CLK);
    write_u8(base, OFF_IS_XTAL_ENABLE, ENABLE_LXT as u8);
    write_u8(base, OFF_IS_RCCAL_IN_L, (!ENABLE_LXT) as u8);

    // BtRomConfig sub-struct.
    let bt = OFF_BT_ROM_CONFIG;
    let bit_valid = VALID_CONTROLLER_ENABLE_BIT
        | VALID_LLD_PROG_DELAY
        | VALID_DEFAULT_SLEEP_MODE
        | VALID_DEFAULT_SLEEP_ENABLED
        | VALID_DEFAULT_XTAL_ENABLED
        | VALID_DEFAULT_RC_CYCLE
        | VALID_IS_FPGA;
    write_u32(base, bt + BT_OFF_BIT_VALID, bit_valid);
    write_u8(base, bt + BT_OFF_CONTROLLER_ENABLE_BIT, 0x03); // BLE + BT both on
    write_u8(base, bt + BT_OFF_LLD_PROG_DELAY, LLD_PROG_DELAY);
    write_u8(base, bt + BT_OFF_DEFAULT_SLEEP_MODE, 0);
    write_u8(base, bt + BT_OFF_DEFAULT_SLEEP_ENABLED, 0); // pm_enabled=false
    write_u8(base, bt + BT_OFF_DEFAULT_XTAL_ENABLED, 1); // xtal_enabled=true
    write_u8(base, bt + BT_OFF_DEFAULT_RC_CYCLE, DEFAULT_RC_CYCLE);
    write_u8(base, bt + BT_OFF_IS_FPGA, 0);

    write_u32(base, OFF_HCPU_IPC_ADDR, hcpu_ipc_addr);
}

/// Write the A3-rev ROM-config block: magic + WDT only. `BtRomConfig`
/// is deferred to `controller::post_init_a3`, which writes it to
/// `G_ROM_CONFIG_A3` after LCPU boots.
fn write_a3() {
    let base = addr::ROM_CONFIG_BASE_A3;

    unsafe {
        ptr::write_bytes(base as *mut u8, 0, addr::ROM_CONFIG_SIZE_A3);
    }

    write_u32(base, OFF_MAGIC, MAGIC_VALUE);
    write_u32(base, OFF_WDT_TIME, WDT_TIME);
    write_u32(base, OFF_WDT_STATUS, WDT_STATUS);
    write_u16(base, OFF_WDT_CLK, WDT_CLK);
    write_u8(base, OFF_IS_XTAL_ENABLE, ENABLE_LXT as u8);
    write_u8(base, OFF_IS_RCCAL_IN_L, (!ENABLE_LXT) as u8);
}

#[inline]
fn write_u32(base: usize, offset: usize, val: u32) {
    let p = (base + offset) as *mut u32;
    unsafe { ptr::write_volatile(p, val) }
}

#[inline]
fn write_u16(base: usize, offset: usize, val: u16) {
    let p = (base + offset) as *mut u16;
    unsafe { ptr::write_volatile(p, val) }
}

#[inline]
fn write_u8(base: usize, offset: usize, val: u8) {
    let p = (base + offset) as *mut u8;
    unsafe { ptr::write_volatile(p, val) }
}
