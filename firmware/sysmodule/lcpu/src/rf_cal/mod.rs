//! Bluetooth RF calibration.
//!
//! Ported from sifli-rs:
//! `sifli-radio/src/bluetooth/rf_cal/mod.rs` @ commit aa4c19c.
//! License: Apache-2.0 (upstream).
//! See `LICENSES/SIFLI-RS-APACHE-2.0.txt`.
//!
//! Adaptations from upstream (orchestrator-level):
//! - `crate::Peripheral`, `crate::dma::Channel`, `embassy_hal_internal`
//!   removed — replaced by an explicit `&mut crate::dma::DmacChannel`
//!   parameter to `bt_rf_cal`. RF cal is single-threaded during
//!   bringup; no need for the type-erased peripheral ownership.
//! - `crate::pac::BT_RFC` → `sifli_pac::BT_RFC`.
//! - `crate::rcc::{lp_rfc_reset_asserted, set_lp_rfc_reset}` are inlined
//!   as direct `LPSYS_RCC.rstr1()` reads/writes (~3 lines).
//! - `sifli_hal::syscfg::ChipRevision` → our `sysmodule_device_api::ChipRev`
//!   (the same revision classification, different crate path).
//! - `sifli_hal::ram::RamSlice` → `crate::ram_slice::RamSlice`.
//! - eFUSE: upstream constructs `Efuse::new(...).calibration()` inline;
//!   we accept a pre-decoded `Bank1Calibration` (from
//!   `sysmodule_efuse_api`) so the IPC call lives in the caller and
//!   this module stays sync. Field accessors `low.edr_cal_done()` etc.
//!   become `cal.edr_cal_done != 0` since our `Bank1Calibration`
//!   stores bools as `u8` for zerocopy/postcard compatibility.
//! - `super::rom_config::set_bt_tx_power(rev, …)` upstream writes a
//!   `BT_TXPWR` field into the LCPU ROM-config block. We don't have a
//!   matching writer in our `crate::rom_config` yet; left as a `TODO`
//!   comment. Cal proceeds without it — the BLE ROM falls back to its
//!   own default TX power table.
//! - All `#[cfg(feature = "edr")]` blocks dropped — BLE-only port. No
//!   `edr_lo` submodule. The EDR LO cal step is omitted from the
//!   sequence.
//! - SDK line citations preserved.

mod consts;
mod opt;
pub mod rfc_cmd;
pub mod rfc_sram;
pub mod rfc_tables;
pub mod txdc;
mod txdc_hw;
pub mod vco;

use crate::addr::{EM_RF_CAL_CLEAR_SIZE, EM_START};
use crate::dma::DmacChannel;
use crate::ram_slice::RamSlice;
use sifli_pac::{BT_RFC, LPSYS_RCC};
use sysmodule_device_api::ChipRev;
use sysmodule_efuse_api::Bank1Calibration;

/// RF driver version: v6.0.0.
const RF_DRIVER_VERSION: u32 = 0x0006_0000;

/// Default EDR PA BM values for each power level (0-7).
///
/// Adjusted based on eFUSE calibration data. See SDK
/// `bt_rfc_pwr_cal_edr()` in `bt_rf_fulcal.c`.
const DEFAULT_EDR_PA_BM: [u8; 8] = [5, 5, 0xE, 0xA, 0x1B, 0x1F, 0x1F, 0x1F];

/// Reset Bluetooth RF module. SDK `HAL_RCC_ResetBluetoothRF`.
fn reset_bluetooth_rf() {
    LPSYS_RCC.rstr1().modify(|w| w.set_rfc(true));
    while !LPSYS_RCC.rstr1().read().rfc() {}
    LPSYS_RCC.rstr1().modify(|w| w.set_rfc(false));
}

/// Apply EDR power calibration from eFUSE Bank1 calibration data.
///
/// Applies calibration to TBB_REG.BRF_DAC_LSB_CNT_LV and the EDR PA BM
/// table. Based on SDK `bt_rfc_pwr_cal_edr()` with `ABS_EDR_CAL`
/// enabled. Returns the adjusted EDR PA BM array iff
/// `cal.edr_cal_done`; otherwise `None`.
///
/// Adaptation from upstream: field accessors `low.edr_cal_done()`
/// etc. are replaced with `cal.edr_cal_done != 0` because our
/// `Bank1Calibration` stores bools as `u8` (for postcard/zerocopy
/// compatibility over the IPC).
pub fn apply_edr_power_cal(cal: &Bank1Calibration) -> Option<[u8; 8]> {
    if cal.edr_cal_done == 0 {
        return None;
    }

    // Apply DAC LSB count calibration to TBB_REG.
    BT_RFC.tbb_reg().modify(|w| {
        w.set_brf_dac_lsb_cnt_lv(cal.dac_lsb_cnt);
    });

    // Adjust EDR PA BM values based on pa_bm.
    let mut edr_pa_bm = DEFAULT_EDR_PA_BM;
    match cal.pa_bm {
        1 => {
            edr_pa_bm[0] = edr_pa_bm[0].saturating_add(1);
            edr_pa_bm[1] = edr_pa_bm[1].saturating_add(2);
            edr_pa_bm[2] = edr_pa_bm[2].saturating_add(2);
            edr_pa_bm[3] = edr_pa_bm[3].saturating_add(2);
            edr_pa_bm[4] = edr_pa_bm[4].saturating_add(4);
        }
        3 => {
            edr_pa_bm[1] = edr_pa_bm[1].saturating_sub(1);
            edr_pa_bm[2] = edr_pa_bm[2].saturating_sub(2);
            edr_pa_bm[3] = edr_pa_bm[3].saturating_sub(2);
            edr_pa_bm[4] = edr_pa_bm[4].saturating_sub(3);
        }
        _ => {}
    }

    Some(edr_pa_bm)
}

/// Get TMXCAP selection values from eFUSE Bank1 calibration data.
/// Returns (tmxcap_ch00, tmxcap_ch78) iff `tmxcap_flag`.
#[allow(dead_code)]
pub fn get_tmxcap_sel(cal: &Bank1Calibration) -> Option<(u8, u8)> {
    if cal.tmxcap_flag == 0 {
        return None;
    }
    Some((cal.tmxcap_ch00, cal.tmxcap_ch78))
}

/// Default BT RF power parameters (max_pwr, min_pwr, init_pwr, is_bqb).
fn default_tx_power_params() -> (i8, i8, i8, u8) {
    (10, 0, 0, 0)
}

/// Power table for calibration level index mapping (dBm).
/// SDK: `pwr_tab[] = {0, 3, 6, 10, 13, 16, 19}` in `bt_rf_cal_index()`.
const PWR_TAB: [i8; 7] = [0, 3, 6, 10, 13, 16, 19];

/// Compute calibration enable bitmask from TX power range.
///
/// Determines which of the 7 power levels need TXDC calibration based
/// on the configured min/max/init TX power. SDK `bt_rf_cal_index()`
/// (bt_rf_fulcal.c:5172).
fn bt_rf_cal_index(min_pwr: i8, max_pwr: i8, init_pwr: i8) -> u8 {
    let effective_max = max_pwr.max(init_pwr);

    let mut min_level: usize = 0;
    for i in (0..PWR_TAB.len()).rev() {
        if PWR_TAB[i] <= min_pwr {
            min_level = i;
            break;
        }
    }

    let mut max_level: usize = PWR_TAB.len() - 1;
    for i in 0..PWR_TAB.len() {
        if PWR_TAB[i] >= effective_max {
            max_level = i;
            break;
        }
    }

    let mut cal_enable: u8 = 0;
    for i in min_level..=max_level {
        cal_enable |= 1 << i;
    }
    cal_enable
}

/// Encode power parameters into 32-bit packed format.
#[allow(dead_code)]
fn encode_tx_power(max: i8, min: i8, init: i8, is_bqb: u8) -> u32 {
    let max_u = max as u8 as u32;
    let min_u = min as u8 as u32;
    let init_u = init as u8 as u32;
    let is_bqb_u = is_bqb as u32;

    (is_bqb_u << 24) | (init_u << 16) | (min_u << 8) | max_u
}

/// Perform Bluetooth RF calibration.
///
/// Corresponds to SDK call chain (BLE-only — EDR LO step omitted):
/// ```text
/// lcpu_ble_patch_install()           // bf0_lcpu_init.c:179
///   ├─ lcpu_patch_install()          // patch (done by caller)
///   ├─ bt_rf_cal()                   // bt_rf_fulcal.c:5451
///   │   ├─ bt_rf_cal_index()         //   compute s_cal_enable mask
///   │   ├─ HAL_RCC_ResetBluetoothRF()
///   │   ├─ bt_rfc_init()             //   RFC regs + command sequences
///   │   ├─ bt_ful_cal(addr)
///   │   │   ├─ bt_rfc_lo_cal()       //   BLE VCO ACAL/FCAL 79ch
///   │   │   └─ bt_rfc_txdc_cal()     //   TX DC offset (DMA-based)
///   │   └─ bt_rf_opt_cal()           //   PHY register optimization
///   └─ memset(EM, 0, 0x5000)         // clear Exchange Memory
/// ```
///
/// `dma_ch` must be a free DMAC2 channel; TXDC uses it for ADC sample
/// capture. `efuse_cal` is the decoded Bank1 calibration read once
/// during bringup via `sysmodule_efuse_api::Efuse::read_calibration()`.
pub fn bt_rf_cal(
    rev: ChipRev,
    dma_ch: &mut DmacChannel,
    efuse_cal: Option<&Bank1Calibration>,
) {
    let _ = rev; // currently only used for the deferred set_bt_tx_power
    // TODO: bt_is_in_BQB_mode() check (SDK:5453) — always assumes non-BQB.
    // SDK:5461 — bt_rf_cal_index(): compute s_cal_enable from power range.
    let (max_pwr, min_pwr, init_pwr, _is_bqb) = default_tx_power_params();
    let cal_enable = bt_rf_cal_index(min_pwr, max_pwr, init_pwr);

    // SDK:5465 — HAL_RCC_ResetBluetoothRF().
    reset_bluetooth_rf();

    // SDK:5471 — PA voltage mode (non-1.8V): clear TMXCAS_SEL.
    BT_RFC.trf_edr_reg1().modify(|w| {
        w.set_brf_trf_edr_tmxcas_sel_lv(false);
    });

    // SDK:5473 — bt_rfc_init(): RFC register init + 6 command sequences → addr.
    vco::rfc_init();
    let sram = rfc_sram::RfcSram::new();
    let cmd_end_addr = rfc_cmd::generate_rfc_cmd_sequences(sram.region());

    // Allocate all calibration table regions upfront.
    let tables = rfc_tables::alloc_cal_tables(&sram, cmd_end_addr);

    // SDK:5072 bt_ful_cal — step a: bt_rfc_lo_cal()
    let vco_cal = vco::vco_cal_full();

    // SDK:5085-5086 bt_ful_cal — step c: LPSYS clock switch before TXDC.
    // Already set to HXT48 / HDIV=2 (= 24 MHz) by
    // `bringup::clock_lcpu_off_hxt48` in our phase 4. TXDC will
    // defensively re-assert this. No additional action here.

    // SDK:5088 bt_ful_cal — step d: bt_rfc_txdc_cal(addr, s_cal_enable).
    // Apply eFUSE-derived EDR power calibration (no-op if
    // `edr_cal_done` is clear or caller passed no cal).
    let edr_pa_bm_opt = efuse_cal.and_then(apply_edr_power_cal);

    // Store VCO cal tables first — force_tx needs CAL_ADDR to look up
    // VCO params.
    rfc_tables::store_vco_cal_tables(&tables, &vco_cal);

    let mut txdc_config = txdc::TxdcCalConfig::default();
    txdc_config.power_level_mask = cal_enable;
    if let Some(pa_bm) = edr_pa_bm_opt {
        txdc_config.edr_pa_bm = pa_bm;
    }

    // TODO: TMXCAP eFUSE calibration (SDK:3818-3842).

    let txdc_cal = txdc::txdc_cal_full(edr_pa_bm_opt, cal_enable, dma_ch);

    // Restore VCO thresholds to normal mode after TXDC cal (SDK:4664-4673).
    BT_RFC.vco_reg2().modify(|w| {
        w.set_brf_vco_acal_vl_sel_lv(consts::VCO_ACAL_VL_NORMAL);
        w.set_brf_vco_acal_vh_sel_lv(consts::VCO_ACAL_VH_NORMAL);
        w.set_brf_vco_incfcal_vl_sel_lv(consts::VCO_INCFCAL_VL);
        w.set_brf_vco_incfcal_vh_sel_lv(consts::VCO_INCFCAL_VH);
    });
    BT_RFC.vco_reg1().modify(|w| {
        w.set_brf_vco_ldo_vref_lv(consts::VCO_LDO_VREF);
    });

    // SDK:5477 — bt_rf_opt_cal().
    opt::bt_rf_opt_cal();

    // SDK:5481 — store driver version (v6.0.0).
    vco::set_driver_version(RF_DRIVER_VERSION);

    // SDK:5488-5492 — save TX power params to LCPU ROM config.
    //
    // TODO: not yet ported. Upstream calls
    // `super::rom_config::set_bt_tx_power(rev, encode_tx_power(...))`,
    // which writes `BT_TXPWR` (offset varies by rev) into the LCPU
    // ROM-config block. Our `crate::rom_config` doesn't have an
    // equivalent writer yet. BLE controller falls back to its built-in
    // default TX power table without this; revisit when actually
    // tuning TX power output.
    let _ = (rev, max_pwr, min_pwr, init_pwr, _is_bqb);

    // Store TXDC cal tables into RFC SRAM. SDK does this inside
    // bt_rfc_txdc_cal; we do it after opt_cal for cleaner ordering.
    rfc_tables::store_txdc_cal_tables(
        sram.region(),
        &tables,
        &txdc_cal,
        &txdc_config.edr_pa_bm,
        &txdc_config.tmxbuf_gc,
    );

    // SDK bf0_lcpu_init.c:208 — clear Exchange Memory (first 20 KiB).
    RamSlice::new(EM_START, EM_RF_CAL_CLEAR_SIZE).clear();
}
