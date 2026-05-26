//! Shared constants for BT RF calibration algorithms.
//!
//! Ported from sifli-rs:
//! `sifli-radio/src/bluetooth/rf_cal/consts.rs` @ commit aa4c19c.
//! License: Apache-2.0 (upstream).
//! See `LICENSES/SIFLI-RS-APACHE-2.0.txt` for the full upstream notice.
//!
//! Adaptations from upstream:
//! - Dropped `FBDV_MOD_STG_3G` — it is `#[cfg(feature = "edr")]`-gated
//!   upstream and we are BLE-only.
//! - Otherwise verbatim. These are SDK-derived register values, no
//!   semantic adaptation possible or appropriate.
//!
//! Constants used by `vco.rs` for 5GHz VCO calibration. (Upstream also
//! shares some of these with `edr_lo.rs`, which we don't port.)

// ============================================================
// IDAC / PDX (capcode) algorithm constants
// ============================================================

/// IDAC binary search initial value (7-bit midpoint).
pub const IDAC_INITIAL: u8 = 0x40;

/// IDAC binary search full-scale value.
pub const IDAC_FS: u8 = 0x40;

/// IDAC maximum value (7-bit: 0x3F = 63).
pub const IDAC_MAX: u8 = 0x3F;

/// PDX (capcode) binary search initial value (8-bit midpoint).
pub const PDX_INITIAL: u8 = 0x80;

/// PDX binary search full-scale value.
pub const PDX_FS: u8 = 0x80;

/// Maximum sweep steps for linear frequency scan.
pub const MAX_LO_CAL_STEP: usize = 256;

// ============================================================
// VCO ACAL threshold pairs
// ============================================================

/// ACAL_VL_SEL during calibration (relaxed thresholds for sweep).
pub const VCO_ACAL_VL_CAL: u8 = 0x1;

/// ACAL_VH_SEL during calibration.
pub const VCO_ACAL_VH_CAL: u8 = 0x3;

/// ACAL_VL_SEL for normal operation (tight thresholds).
pub const VCO_ACAL_VL_NORMAL: u8 = 0x5;

/// ACAL_VH_SEL for normal operation.
pub const VCO_ACAL_VH_NORMAL: u8 = 0x7;

/// INCFCAL_VL_SEL for normal operation.
pub const VCO_INCFCAL_VL: u8 = 0x2;

/// INCFCAL_VH_SEL for normal operation.
pub const VCO_INCFCAL_VH: u8 = 0x5;

/// VCO LDO voltage reference setting.
pub const VCO_LDO_VREF: u8 = 0xA;

// ============================================================
// PHY FCW (Frequency Control Word) values
// ============================================================

/// LFP_FCW value for calibration (PAC field is u16).
pub const LFP_FCW_CAL: u16 = 0x08;

/// HFP_FCW value for calibration (PAC field is u8).
pub const HFP_FCW_CAL: u8 = 0x07;

// ============================================================
// FBDV modulator stage configuration
// ============================================================

/// FBDV modulator stage for 5GHz VCO (BLE).
pub const FBDV_MOD_STG_5G: u8 = 2;

// ============================================================
// Sequential ACAL termination thresholds
// ============================================================

/// Sequential ACAL: max direction-change count before termination.
pub const SEQ_ACAL_JUMP_LIMIT: u8 = 4;

/// Sequential ACAL: max full-scale (rail) count before termination.
pub const SEQ_ACAL_FULL_LIMIT: u8 = 2;
