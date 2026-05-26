//! RFC calibration table storage in SRAM.
//!
//! Ported from sifli-rs:
//! `sifli-radio/src/bluetooth/rf_cal/rfc_tables.rs` @ commit aa4c19c.
//! License: Apache-2.0 (upstream).
//! See `LICENSES/SIFLI-RS-APACHE-2.0.txt`.
//!
//! Adaptations from upstream:
//! - `crate::pac::BT_RFC` → `sifli_pac::BT_RFC` (same chip PAC, different
//!   crate root since we use sifli-pac directly rather than through
//!   sifli-hal's re-export).
//! - `sifli_hal::ram::RamSlice` → `crate::ram_slice::RamSlice`.
//! - Dropped `store_edr_lo_cal_tables` and the `#[cfg(feature = "edr")]`
//!   imports of `edr_lo::*` — we are BLE-only.
//! - Body of `store_vco_cal_tables` / `store_txdc_cal_tables` / signatures
//!   unchanged.
//!
//! Stores VCO and TXDC calibration result tables into RFC SRAM so the
//! BLE MAC can load per-channel VCO parameters (via `RD_FULCAL`) and
//! per-power-level TXDC parameters (via `RD_DCCAL1`/`RD_DCCAL2`) from
//! tables addressed by `CAL_ADDR_REG1/2/3`.

use super::rfc_sram::{RfcSram, RfcTable};
use super::txdc::{NUM_POWER_LEVELS, TxdcCalResult};
use super::vco::VcoCalResult;
use crate::ram_slice::RamSlice;
use rwbt::rfc::sifli::cal_table::{pack_txdc, pack_vco_rx_half, pack_vco_rx_word, pack_vco_tx};
use sifli_pac::BT_RFC;

/// All calibration tables allocated in RFC SRAM.
pub struct CalTables {
    /// BLE RX calibration table (40 words, 1M+2M packed per word).
    pub ble_rx: RfcTable,
    /// BT RX calibration table (40 words, 79 channels packed as pairs).
    pub bt_rx: RfcTable,
    /// BLE TX calibration table (79 words, one per channel).
    pub ble_tx: RfcTable,
    /// BT TX calibration table (79 words). EDR LO overrides this when
    /// the `edr` feature is enabled upstream; we never overwrite it.
    pub bt_tx: RfcTable,
    /// TXDC calibration table (16 words = 8 power levels × 2 words).
    pub txdc: RfcTable,
}

/// Allocate all calibration tables sequentially from `cmd_end_addr`.
///
/// Must be called after `generate_rfc_cmd_sequences()` which determines
/// `cmd_end_addr`. Tables are allocated in fixed order: BLE RX → BT RX
/// → BLE TX → BT TX → TXDC.
pub fn alloc_cal_tables(sram: &RfcSram, cmd_end_addr: u32) -> CalTables {
    let (ble_rx, next) = sram.alloc_table(cmd_end_addr, 40);
    let (bt_rx, next) = sram.alloc_table(next, 40);
    let (ble_tx, next) = sram.alloc_table(next, 79);
    let (bt_tx, next) = sram.alloc_table(next, 79);
    let (txdc, _) = sram.alloc_table(next, 16);
    CalTables {
        ble_rx,
        bt_rx,
        ble_tx,
        bt_tx,
        txdc,
    }
}

/// Write VCO calibration tables (RX + TX) to RFC SRAM and update
/// `CAL_ADDR_REG1`/`CAL_ADDR_REG2`.
///
/// Must be called after VCO calibration and BEFORE TXDC calibration —
/// TXDC's `force_tx` needs `CAL_ADDR` to look up the correct VCO
/// parameters for the forced channel.
pub fn store_vco_cal_tables(tables: &CalTables, vco_cal: &VcoCalResult) {
    // === BLE RX calibration table (40 words) ===
    for i in 0..40 {
        let rx_1m = pack_vco_rx_half(vco_cal.capcode_rx_1m[i], vco_cal.idac_rx_1m[i]);
        let rx_2m = pack_vco_rx_half(vco_cal.capcode_rx_2m[i], vco_cal.idac_rx_2m[i]);
        tables.ble_rx.write(i, pack_vco_rx_word(rx_1m, rx_2m));
    }
    // === BT RX calibration table (40 words, packing 79 channels as pairs) ===
    for i in 0..40 {
        let ch0 = 2 * i;
        let ch1 = 2 * i + 1;
        let lo = pack_vco_rx_half(vco_cal.capcode_rx_bt[ch0], vco_cal.idac_rx_bt[ch0]);
        let hi = if ch1 < 79 {
            pack_vco_rx_half(vco_cal.capcode_rx_bt[ch1], vco_cal.idac_rx_bt[ch1])
        } else {
            lo // last odd channel: duplicate
        };
        tables.bt_rx.write(i, pack_vco_rx_word(lo, hi));
    }
    BT_RFC.cal_addr_reg1().write(|w| {
        w.set_ble_rx_cal_addr(tables.ble_rx.offset());
        w.set_bt_rx_cal_addr(tables.bt_rx.offset());
    });

    // === BLE TX calibration table (79 words) ===
    for i in 0..79 {
        tables.ble_tx.write(
            i,
            pack_vco_tx(vco_cal.capcode_tx[i], vco_cal.idac_tx[i], vco_cal.kcal[i]),
        );
    }
    // === BT TX calibration table (79 words) -- same as BLE TX ===
    for i in 0..79 {
        tables.bt_tx.write(
            i,
            pack_vco_tx(vco_cal.capcode_tx[i], vco_cal.idac_tx[i], vco_cal.kcal[i]),
        );
    }
    BT_RFC.cal_addr_reg2().write(|w| {
        w.set_ble_tx_cal_addr(tables.ble_tx.offset());
        w.set_bt_tx_cal_addr(tables.bt_tx.offset());
    });
}

/// Write TXDC calibration tables to RFC SRAM and update `CAL_ADDR_REG3`.
///
/// Called after TXDC calibration with the calibration results.
/// `sram_region` is the full RFC SRAM `RamSlice` (from
/// `RfcSram::region()`), used to overwrite BT_TXON EDR calibration
/// commands.
pub fn store_txdc_cal_tables(
    sram_region: &RamSlice,
    tables: &CalTables,
    txdc_cal: &TxdcCalResult,
    edr_pa_bm: &[u8; 8],
    tmxbuf_gc: &[u8; 8],
) {
    // === TXDC calibration table (8 power levels × 2 words = 16 words) ===
    for level in 0..8usize {
        // SDK mapping: m=i; if(i>4) m=i-1  ->  [0,1,2,3,4,4,5,6]
        let m = if level > 4 { level - 1 } else { level };
        let m = m.min(NUM_POWER_LEVELS - 1);
        let pt = &txdc_cal.points[m];

        let entry = pack_txdc(
            pt.coef0,
            pt.coef1,
            pt.offset_i,
            pt.offset_q,
            tmxbuf_gc[level],
            edr_pa_bm[level],
        );
        tables.txdc.write_pair(level, entry.word1, entry.word2);
    }
    BT_RFC.cal_addr_reg3().write(|w| {
        w.set_txdc_cal_addr(tables.txdc.offset());
    });

    // Replace EDR cal related commands in BT_TXON with WAIT commands.
    // SDK: `bt_rfc_txdc_cal` lines 5001-5009.

    /// Number of command words to overwrite in the BT_TXON EDR
    /// calibration section.
    const BT_TXON_EDR_CAL_CMD_WORDS: usize = 10;
    /// Byte offset of the EDR calibration commands within the BT_TXON
    /// sequence.
    const BT_TXON_EDR_CAL_OFFSET: usize = 32 * 4;

    let wait1 = rwbt::rfc::cmd::wait(1) as u32;
    let wait1_pair = wait1 | (wait1 << 16); // Two WAIT(1) commands packed per word
    let bt_txon_addr = BT_RFC.cu_addr_reg3().read().bt_txon_cfg_addr() as usize;
    let overwrite_region = sram_region.slice(
        bt_txon_addr + BT_TXON_EDR_CAL_OFFSET,
        BT_TXON_EDR_CAL_CMD_WORDS * 4,
    );
    for i in 0..BT_TXON_EDR_CAL_CMD_WORDS {
        overwrite_region.write::<u32>(i * 4, wait1_pair);
    }
}
