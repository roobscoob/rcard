//! LCPU shared-memory address map.
//!
//! Single source of truth for the addresses the bringup steps poke. All
//! values are HCPU virtual addresses unless suffixed `_LCPU`. The LCPU
//! sees HCPU SRAM at `addr + HCPU_TO_LCPU_OFFSET` and LPSYS RAM at
//! `addr - LPSYS_RAM_HCPU_OFFSET`.
//!
//! Sourced from sifli-rs `sifli-hal/data/sf32lb52x/sram_layout.toml`.

use sysmodule_syscon_api::ChipRev;

/// HCPU→LCPU virtual-address translation: an HCPU SRAM address `X`
/// appears to LCPU at `X + HCPU_TO_LCPU_OFFSET`. Used when LCPU needs
/// to read HCPU-side memory (e.g. the HCPU→LCPU mailbox TX buffer).
pub const HCPU_TO_LCPU_OFFSET: usize = 0x0A00_0000;

/// Offset between the HCPU and LCPU views of LPSYS_RAM. The LCPU sees
/// LPSYS_RAM at its native base (`0x0040_0000`); HCPU sees the same
/// physical memory at `0x2040_0000`. To convert an HCPU LPSYS_RAM
/// pointer into the LCPU's view, *subtract* this offset.
pub const LPSYS_RAM_HCPU_OFFSET: usize = 0x2000_0000;

/// LCPU SRAM base in HCPU virtual address space.
pub const LPSYS_RAM_BASE: usize = 0x2040_0000;

/// NVDS TLV blob (LPSYS_SRAM region). Same address on A3 and Letter.
pub const NVDS_BUFF_START: usize = 0x2040_FE00;

// ── Letter revision ─────────────────────────────────────────────────

/// Letter-rev ROM-config block (204 B; full struct including BT_ROM_CONFIG).
pub const ROM_CONFIG_BASE_LETTER: usize = 0x2040_2A00;
pub const ROM_CONFIG_SIZE_LETTER: usize = 0xCC;

/// Letter-rev patch buffer header (`PACH` magic + 7 + entry-point) at
/// `0x2040_5000`. Patch code starts immediately after at `+0x000C`.
pub const PATCH_BUF_START_LETTER: usize = 0x2040_5000;
pub const PATCH_CODE_START_LETTER: usize = 0x2040_500C;
pub const PATCH_CODE_SIZE_LETTER: usize = 0x2FF4;
/// LCPU-view of `PATCH_CODE_START_LETTER`. Used inside the PACH header
/// so the LCPU ROM resolves the code at its own translated address.
/// LPSYS_RAM is shared physical memory — LCPU sees it
/// `LPSYS_RAM_HCPU_OFFSET` lower than HCPU does, so subtract.
pub const PATCH_CODE_START_LCPU_LETTER: u32 =
    (PATCH_CODE_START_LETTER - LPSYS_RAM_HCPU_OFFSET) as u32;

/// Letter-rev LCPU→HCPU mailbox CH1 ring (HCPU view).
/// LCPU writes here; HCPU reads.
pub const LCPU2HCPU_MB_CH1_LETTER: usize = 0x2040_2800;

// ── A3 revision ──────────────────────────────────────────────────────

/// A3 LPSYS RAM region size — the firmware blob copied to
/// `LPSYS_RAM_BASE` must fit in this window. 24 KiB per SDK layout.
pub const A3_LPSYS_RAM_SIZE: usize = 0x6000;

/// A3-rev ROM-config block (only 64 B used; magic + WDT fields only;
/// `BtRomConfig` is written post-boot in `controller::post_init_a3`).
pub const ROM_CONFIG_BASE_A3: usize = 0x2040_FDC0;
pub const ROM_CONFIG_SIZE_A3: usize = 0x40;

/// A3 patch record list table — copy of `patch_a3_list.bin` lives here
/// because A3's PATCH peripheral reads entries from RAM, not flash.
pub const PATCH_RECORD_ADDR_A3: usize = 0x2040_7F00;
pub const PATCH_RECORD_SIZE_A3: usize = 0x100;

/// A3 patch code area — copy of `patch_a3_bin.bin` lives here.
pub const PATCH_CODE_START_A3: usize = 0x2040_6000;
pub const PATCH_CODE_SIZE_A3: usize = 0x2000;

/// A3-rev LCPU→HCPU mailbox CH1 ring (HCPU view).
pub const LCPU2HCPU_MB_CH1_A3: usize = 0x2040_5C00;

/// A3 post-boot ROM-runtime variable addresses written by `controller::post_init_a3`.
/// `lld_prog_delay` (u8).
pub const RWIP_PROG_DELAY_A3: usize = 0x2040_FA94;
/// `g_rom_config` — 24-byte `BtRomConfig` matching the Letter
/// BT_ROM_CONFIG sub-struct layout.
pub const G_ROM_CONFIG_A3: usize = 0x2040_E48C;

// ── Common ───────────────────────────────────────────────────────────

/// IPC ring-buffer size (header + payload). 512 B per SDK convention.
pub const IPC_MB_BUF_SIZE: usize = 0x200;

// ── RF calibration ───────────────────────────────────────────────────
//
// Address constants used by the vendored `rf_cal/*` modules. Mirrors
// sifli-rs `sifli-hal/data/sf32lb52x/sram_layout.toml` @ aa4c19c.

/// BT_RFC internal SRAM base (peripheral bus address). Holds the 6 MAC
/// command sequences (RXON/OFF, TXON/OFF, BT_TXON/OFF) and the per-channel
/// VCO + TXDC calibration tables written by `rf_cal/rfc_tables.rs`. The
/// MAC reads from here during TX/RX via `CU_ADDR_REG` / `CAL_ADDR_REG`
/// offsets. Inside `lpsys` aggregate (covered by lcpu's MPU).
///
/// Source: sram_layout.toml banks.regions @ 0x40082000, size 0x800.
pub const BT_RFC_MEM_BASE: u32 = 0x4008_2000;
pub const BT_RFC_SRAM_SIZE: u32 = 0x800;

/// PHY RX dump buffer source for the TXDC DMA capture. CPU direct reads
/// of this address bus-fault — only DMA may read it. Inside `lpsys`
/// aggregate.
///
/// Source: sram_layout.toml banks.regions @ 0x400C0000, size 0x4000.
pub const PHY_RX_DUMP_ADDR: u32 = 0x400C_0000;

/// LPSYS Exchange Memory start (HCPU view). The BLE MAC's link-layer
/// scheduler stores Activity CS, TX/RX descriptors, and ADV/ACL buffers
/// here. RF cal clears the first `EM_RF_CAL_CLEAR_SIZE` of it on
/// completion. Inside `lpsys_sram`.
///
/// Source: sram_layout.toml LPSYS_EM_BASE = 0x2040_8000.
pub const EM_START: usize = 0x2040_8000;

/// Byte count the SDK `bt_rf_cal()` memsets to zero at the end of cal.
/// The full EM is 24 KiB but only the first 20 KiB is cleared per
/// `bf0_lcpu_init.c:155`.
pub const EM_RF_CAL_CLEAR_SIZE: usize = 0x5000;

/// EM scratch region TXDC's DMA writes captured ADC samples into.
/// `txdc::DMA_BUFFER_ADDR` in sifli-rs aliases this. Same address as
/// `EM_START` because the TXDC capture transiently overwrites the
/// beginning of EM (which we then clear at end of cal anyway).
pub const TXDC_DMA_BUFFER_ADDR: u32 = EM_START as u32;

/// Pick the LCPU→HCPU mailbox CH1 ring address for the detected chip rev.
pub const fn lcpu2hcpu_mb_ch1(rev: ChipRev) -> usize {
    match rev {
        ChipRev::Letter => LCPU2HCPU_MB_CH1_LETTER,
        ChipRev::A3OrEarlier => LCPU2HCPU_MB_CH1_A3,
    }
}
