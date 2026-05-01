//! LCPU shared-memory address map (Letter revision).
//!
//! Single source of truth for the addresses the bringup steps poke. All
//! values are HCPU virtual addresses unless suffixed `_LCPU`. The LCPU
//! sees HCPU addresses translated by `+HCPU_TO_LCPU_OFFSET`.
//!
//! Sourced from sifli-rs `sifli-hal/data/sf32lb52x/sram_layout.toml`.

/// HCPU→LCPU virtual-address translation: an HCPU SRAM address `X`
/// appears to LCPU at `X + HCPU_TO_LCPU_OFFSET`.
pub const HCPU_TO_LCPU_OFFSET: usize = 0x0A00_0000;

/// LCPU SRAM base in HCPU virtual address space.
pub const LPSYS_RAM_BASE: usize = 0x2040_0000;

/// NVDS TLV blob (LPSYS_SRAM region).
pub const NVDS_BUFF_START: usize = 0x2040_FE00;

/// Letter-rev ROM-config block.
pub const ROM_CONFIG_BASE_LETTER: usize = 0x2040_2A00;
/// Total bytes the ROM-config block reserves (Letter rev). Includes the
/// padding tail; `apply()` only writes specific offsets.
pub const ROM_CONFIG_SIZE_LETTER: usize = 0xCC;

/// Patch buffer header (`PACH` magic + 7 + entry-point) at `0x2040_5000`.
/// Patch code starts immediately after at `+0x000C`.
pub const PATCH_BUF_START: usize = 0x2040_5000;
pub const PATCH_CODE_START: usize = 0x2040_500C;
/// LCPU-view of `PATCH_CODE_START`. Used inside the PACH header so the
/// LCPU ROM resolves the code at its own translated address.
pub const PATCH_CODE_START_LCPU: u32 = (PATCH_CODE_START - HCPU_TO_LCPU_OFFSET) as u32;
pub const PATCH_CODE_SIZE: usize = 0x2FF4;

/// LCPU→HCPU mailbox CH1 ring buffer (HCPU view).
/// LCPU writes here; HCPU reads.
pub const LCPU2HCPU_MB_CH1: usize = 0x2040_5C00;

/// IPC ring-buffer size (header + payload). 512 B per SDK convention.
pub const IPC_MB_BUF_SIZE: usize = 0x200;
