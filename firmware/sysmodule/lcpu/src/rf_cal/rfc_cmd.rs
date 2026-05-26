//! RFC command sequence generation for BLE MAC hardware.
//!
//! Ported from sifli-rs:
//! `sifli-radio/src/bluetooth/rf_cal/rfc_cmd.rs` @ commit aa4c19c.
//! License: Apache-2.0 (upstream).
//! See `LICENSES/SIFLI-RS-APACHE-2.0.txt`.
//!
//! Adaptations from upstream:
//! - `crate::pac::BT_RFC` → `sifli_pac::BT_RFC`.
//! - `sifli_hal::ram::RamSlice` → `crate::ram_slice::RamSlice` (verbatim
//!   port; same semantics).
//! - All `rwbt::*` imports are unchanged — `rwbt` is now a direct
//!   dependency of `sysmodule_lcpu` (see Cargo.toml).
//! - Otherwise verbatim. The `build_*` functions are pure `CmdBuilder`
//!   composition; no semantic adaptation is possible. Comments and SDK
//!   line citations preserved.
//!
//! The BLE MAC controller automatically executes RFC command sequences
//! during TX/RX operations to control the RF front-end (power, PLL,
//! VCO, ADC, etc.). These sequences are stored in RFC SRAM and their
//! addresses written to `CU_ADDR_REG1/2/3`. Without them, the MAC
//! cannot operate the RF front-end and no BLE packets will be
//! transmitted or received.
//!
//! Based on SDK `bt_rfc_init()` in `bt_rf_fulcal.c`.

use crate::ram_slice::RamSlice;
use rwbt::rfc::cmd::{self, CmdBuilder};
use rwbt::rfc::sifli::regs::{
    adc_reg, fbdv_reg1, inccal_reg1, offset as reg, oslo_reg, pfdcp_reg, rbb_reg1, rbb_reg2,
    rbb_reg3, rbb_reg5, rf_lodist_reg, rrf_reg, tbb_reg, trf_edr_reg1, trf_edr_reg2, trf_reg1,
    trf_reg2, vco_reg1, vco_reg2,
};
use sifli_pac::BT_RFC;

/// Build the RXON command sequence (BLE RX startup).
fn build_rxon() -> CmdBuilder {
    let mut c = CmdBuilder::new();

    // VDDPSW/RFBG_EN/LO_IARY_EN
    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::or(rf_lodist_reg::BRF_EN_VDDPSW_LV));
    c.push(cmd::or(rf_lodist_reg::BRF_EN_RFBG_LV));
    c.push(cmd::or(rf_lodist_reg::BRF_LO_IARY_EN_LV));
    c.push(cmd::wr(reg::RF_LODIST_REG));

    c.push(cmd::wait(2));

    // FULCAL RSLT
    c.push(cmd::RD_FULCAL);
    c.push(cmd::wr(reg::VCO_REG3));

    // VCO5G_EN
    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::or(vco_reg1::BRF_VCO5G_EN_LV));
    c.push(cmd::wr(reg::VCO_REG1));

    // PFDCP_EN
    c.push(cmd::rd(reg::PFDCP_REG));
    c.push(cmd::or(pfdcp_reg::BRF_PFDCP_EN_LV));
    c.push(cmd::wr(reg::PFDCP_REG));

    // FBDV_EN
    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::or(fbdv_reg1::BRF_FBDV_EN_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    // FBDV_RSTB (clear bit 7)
    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::and(fbdv_reg1::BRF_FBDV_RSTB_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    // wait 30us for LO lock
    c.push(cmd::wait(45));

    // VCO_FLT_EN
    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::or(vco_reg1::BRF_VCO_FLT_EN_LV));
    c.push(cmd::wr(reg::VCO_REG1));

    // LDO11_EN & LNA_SHUNTSW (clear)
    c.push(cmd::rd(reg::RRF_REG));
    c.push(cmd::or(rrf_reg::BRF_RRF_LDO11_EN_LV));
    c.push(cmd::and(rrf_reg::BRF_LNA_SHUNTSW_LV));
    c.push(cmd::wr(reg::RRF_REG));

    // ADC: LDO_ADCREF, LDO_ADC, ADC_I, ADC_Q
    c.push(cmd::rd(reg::ADC_REG));
    c.push(cmd::or(adc_reg::BRF_EN_LDO_ADCREF_LV));
    c.push(cmd::or(adc_reg::BRF_EN_LDO_ADC_LV));
    c.push(cmd::or(adc_reg::BRF_EN_ADC_I_LV));
    c.push(cmd::or(adc_reg::BRF_EN_ADC_Q_LV));
    c.push(cmd::wr(reg::ADC_REG));

    // LDO_RBB
    c.push(cmd::rd(reg::RBB_REG1));
    c.push(cmd::or(rbb_reg1::BRF_EN_LDO_RBB_LV));
    c.push(cmd::wr(reg::RBB_REG1));

    // PA_TX_RX (clear — set to RX mode)
    c.push(cmd::rd(reg::TRF_REG2));
    c.push(cmd::and(trf_reg2::BRF_PA_TX_RX_LV));
    c.push(cmd::wr(reg::TRF_REG2));

    // EN_IARRAY & EN_OSDACQ & EN_OSDACI
    c.push(cmd::rd(reg::RBB_REG5));
    c.push(cmd::or(rbb_reg5::BRF_EN_IARRAY_LV));
    c.push(cmd::or(rbb_reg5::BRF_EN_OSDACQ_LV));
    c.push(cmd::or(rbb_reg5::BRF_EN_OSDACI_LV));
    c.push(cmd::wr(reg::RBB_REG5));

    // EN_CBPF & EN_RVGA_Q & EN_RVGA_I
    c.push(cmd::rd(reg::RBB_REG2));
    c.push(cmd::or(rbb_reg2::BRF_EN_CBPF_LV));
    c.push(cmd::or(rbb_reg2::BRF_EN_RVGA_Q_LV));
    c.push(cmd::or(rbb_reg2::BRF_EN_RVGA_I_LV));
    c.push(cmd::wr(reg::RBB_REG2));

    // EN_PKDET (4-bit field)
    c.push(cmd::rd(reg::RBB_REG3));
    c.push(cmd::or(rbb_reg3::BRF_EN_PKDET_LV));
    c.push(cmd::or(rbb_reg3::BRF_EN_PKDET_LV + 1));
    c.push(cmd::or(rbb_reg3::BRF_EN_PKDET_LV + 2));
    c.push(cmd::or(rbb_reg3::BRF_EN_PKDET_LV + 3));
    c.push(cmd::wr(reg::RBB_REG3));

    c.push(cmd::wait(5));

    // LODIST5G_RX_EN
    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::or(rf_lodist_reg::BRF_LODIST5G_RX_EN_LV));
    c.push(cmd::wr(reg::RF_LODIST_REG));

    // LNA_PU & MX_PU
    c.push(cmd::rd(reg::RRF_REG));
    c.push(cmd::or(rrf_reg::BRF_MX_PU_LV));
    c.push(cmd::or(rrf_reg::BRF_LNA_PU_LV));
    c.push(cmd::wr(reg::RRF_REG));

    // START INCCAL
    c.push(cmd::rd(reg::INCCAL_REG1));
    c.push(cmd::or(inccal_reg1::INCCAL_START));
    c.push(cmd::wr(reg::INCCAL_REG1));

    c.push(cmd::wait(30));

    c.push(cmd::END);
    c.pad_even();
    c
}

/// Build the RXOFF command sequence (BLE RX shutdown).
fn build_rxoff() -> CmdBuilder {
    let mut c = CmdBuilder::new();

    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::and(rf_lodist_reg::BRF_EN_VDDPSW_LV));
    c.push(cmd::and(rf_lodist_reg::BRF_EN_RFBG_LV));
    c.push(cmd::and(rf_lodist_reg::BRF_LO_IARY_EN_LV));
    c.push(cmd::and(rf_lodist_reg::BRF_LODIST5G_RX_EN_LV));
    c.push(cmd::wr(reg::RF_LODIST_REG));

    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::and(vco_reg1::BRF_VCO5G_EN_LV));
    c.push(cmd::and(vco_reg1::BRF_VCO_FLT_EN_LV));
    c.push(cmd::wr(reg::VCO_REG1));

    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::and(fbdv_reg1::BRF_FBDV_EN_LV));
    c.push(cmd::or(fbdv_reg1::BRF_FBDV_RSTB_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    c.push(cmd::rd(reg::PFDCP_REG));
    c.push(cmd::and(pfdcp_reg::BRF_PFDCP_EN_LV));
    c.push(cmd::wr(reg::PFDCP_REG));

    c.push(cmd::rd(reg::RRF_REG));
    c.push(cmd::and(rrf_reg::BRF_MX_PU_LV));
    c.push(cmd::or(rrf_reg::BRF_LNA_SHUNTSW_LV));
    c.push(cmd::and(rrf_reg::BRF_LNA_PU_LV));
    c.push(cmd::and(rrf_reg::BRF_RRF_LDO11_EN_LV));
    c.push(cmd::wr(reg::RRF_REG));

    c.push(cmd::rd(reg::ADC_REG));
    c.push(cmd::and(adc_reg::BRF_EN_LDO_ADCREF_LV));
    c.push(cmd::and(adc_reg::BRF_EN_LDO_ADC_LV));
    c.push(cmd::and(adc_reg::BRF_EN_ADC_I_LV));
    c.push(cmd::and(adc_reg::BRF_EN_ADC_Q_LV));
    c.push(cmd::wr(reg::ADC_REG));

    c.push(cmd::rd(reg::RBB_REG1));
    c.push(cmd::and(rbb_reg1::BRF_EN_LDO_RBB_LV));
    c.push(cmd::wr(reg::RBB_REG1));

    c.push(cmd::rd(reg::TRF_REG2));
    c.push(cmd::or(trf_reg2::BRF_PA_TX_RX_LV));
    c.push(cmd::wr(reg::TRF_REG2));

    c.push(cmd::rd(reg::RBB_REG5));
    c.push(cmd::and(rbb_reg5::BRF_EN_IARRAY_LV));
    c.push(cmd::and(rbb_reg5::BRF_EN_OSDACQ_LV));
    c.push(cmd::and(rbb_reg5::BRF_EN_OSDACI_LV));
    c.push(cmd::wr(reg::RBB_REG5));

    c.push(cmd::rd(reg::RBB_REG2));
    c.push(cmd::and(rbb_reg2::BRF_EN_CBPF_LV));
    c.push(cmd::and(rbb_reg2::BRF_EN_RVGA_Q_LV));
    c.push(cmd::and(rbb_reg2::BRF_EN_RVGA_I_LV));
    c.push(cmd::wr(reg::RBB_REG2));

    c.push(cmd::rd(reg::RBB_REG3));
    c.push(cmd::and(rbb_reg3::BRF_EN_PKDET_LV));
    c.push(cmd::and(rbb_reg3::BRF_EN_PKDET_LV + 1));
    c.push(cmd::and(rbb_reg3::BRF_EN_PKDET_LV + 2));
    c.push(cmd::and(rbb_reg3::BRF_EN_PKDET_LV + 3));
    c.push(cmd::wr(reg::RBB_REG3));

    c.push(cmd::END);
    c.pad_even();
    c
}

/// Build the TXON command sequence (BLE TX startup).
fn build_txon() -> CmdBuilder {
    let mut c = CmdBuilder::new();

    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::or(rf_lodist_reg::BRF_EN_RFBG_LV));
    c.push(cmd::or(rf_lodist_reg::BRF_EN_VDDPSW_LV));
    c.push(cmd::or(rf_lodist_reg::BRF_LO_IARY_EN_LV));
    c.push(cmd::wr(reg::RF_LODIST_REG));

    c.push(cmd::wait(2));

    c.push(cmd::RD_FULCAL);
    c.push(cmd::wr(reg::VCO_REG3));

    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::or(vco_reg1::BRF_VCO5G_EN_LV));
    c.push(cmd::wr(reg::VCO_REG1));

    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::or(fbdv_reg1::BRF_FBDV_EN_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    c.push(cmd::rd(reg::PFDCP_REG));
    c.push(cmd::or(pfdcp_reg::BRF_PFDCP_EN_LV));
    c.push(cmd::wr(reg::PFDCP_REG));

    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::and(fbdv_reg1::BRF_FBDV_RSTB_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    c.push(cmd::wait(30));

    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::or(vco_reg1::BRF_VCO_FLT_EN_LV));
    c.push(cmd::wr(reg::VCO_REG1));

    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::or(rf_lodist_reg::BRF_LODIST5G_BLETX_EN_LV));
    c.push(cmd::wr(reg::RF_LODIST_REG));

    c.push(cmd::rd(reg::TRF_EDR_REG1));
    c.push(cmd::or(trf_edr_reg1::BRF_TRF_EDR_IARRAY_EN_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG1));

    c.push(cmd::rd(reg::TRF_REG1));
    c.push(cmd::or(trf_reg1::BRF_PA_BUF_PU_LV));
    c.push(cmd::wr(reg::TRF_REG1));

    c.push(cmd::rd(reg::TRF_EDR_REG2));
    c.push(cmd::and(trf_edr_reg2::BRF_TRF_EDR_PA_XFMR_SG_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG2));

    c.push(cmd::wait(5));

    c.push(cmd::rd(reg::TRF_REG1));
    c.push(cmd::or(trf_reg1::BRF_TRF_SIG_EN_LV));
    c.push(cmd::or(trf_reg1::BRF_PA_OUT_PU_LV));
    c.push(cmd::wr(reg::TRF_REG1));

    c.push(cmd::rd(reg::INCCAL_REG1));
    c.push(cmd::or(inccal_reg1::INCCAL_START));
    c.push(cmd::wr(reg::INCCAL_REG1));
    c.push(cmd::wait(9));

    c.push(cmd::END);
    c.pad_even();
    c
}

/// Build the TXOFF command sequence (BLE TX shutdown).
fn build_txoff() -> CmdBuilder {
    let mut c = CmdBuilder::new();

    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::and(rf_lodist_reg::BRF_LODIST5G_BLETX_EN_LV));
    c.push(cmd::and(rf_lodist_reg::BRF_LO_IARY_EN_LV));
    c.push(cmd::and(rf_lodist_reg::BRF_EN_RFBG_LV));
    c.push(cmd::and(rf_lodist_reg::BRF_EN_VDDPSW_LV));
    c.push(cmd::wr(reg::RF_LODIST_REG));

    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::and(vco_reg1::BRF_VCO5G_EN_LV));
    c.push(cmd::and(vco_reg1::BRF_VCO_FLT_EN_LV));
    c.push(cmd::wr(reg::VCO_REG1));

    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::and(fbdv_reg1::BRF_FBDV_EN_LV));
    c.push(cmd::or(fbdv_reg1::BRF_FBDV_RSTB_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    c.push(cmd::rd(reg::PFDCP_REG));
    c.push(cmd::and(pfdcp_reg::BRF_PFDCP_EN_LV));
    c.push(cmd::wr(reg::PFDCP_REG));

    c.push(cmd::rd(reg::TRF_REG1));
    c.push(cmd::and(trf_reg1::BRF_PA_BUF_PU_LV));
    c.push(cmd::and(trf_reg1::BRF_TRF_SIG_EN_LV));
    c.push(cmd::and(trf_reg1::BRF_PA_OUT_PU_LV));
    c.push(cmd::wr(reg::TRF_REG1));

    c.push(cmd::rd(reg::TRF_EDR_REG1));
    c.push(cmd::and(trf_edr_reg1::BRF_TRF_EDR_IARRAY_EN_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG1));

    c.push(cmd::rd(reg::TBB_REG));
    c.push(cmd::and(tbb_reg::BRF_EN_TBB_IARRAY_LV));
    c.push(cmd::and(tbb_reg::BRF_EN_LDO_DAC_DVDD_LV));
    c.push(cmd::and(tbb_reg::BRF_EN_LDO_DAC_AVDD_LV));
    c.push(cmd::and(tbb_reg::BRF_EN_DAC_LV));
    c.push(cmd::and(tbb_reg::BRF_DAC_START_LV));
    c.push(cmd::wr(reg::TBB_REG));

    c.push(cmd::rd(reg::TRF_EDR_REG2));
    c.push(cmd::and(trf_edr_reg2::BRF_TRF_EDR_PA_XFMR_SG_LV));
    c.push(cmd::and(trf_edr_reg2::BRF_TRF_EDR_PACAP_EN_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG2));

    c.push(cmd::rd(reg::TRF_EDR_REG1));
    c.push(cmd::and(trf_edr_reg1::BRF_TRF_EDR_PA_PU_LV));
    c.push(cmd::and(trf_edr_reg1::BRF_TRF_EDR_TMX_PU_LV));
    c.push(cmd::and(trf_edr_reg1::BRF_TRF_EDR_TMXBUF_PU_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG1));

    c.push(cmd::rd(reg::OSLO_REG));
    c.push(cmd::and(oslo_reg::BRF_OSLO_EN_LV));
    c.push(cmd::wr(reg::OSLO_REG));

    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::and(vco_reg1::BRF_VCO3G_EN_LV));
    c.push(cmd::and(vco_reg1::BRF_VCO_FLT_EN_LV));
    c.push(cmd::wr(reg::VCO_REG1));

    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::or(fbdv_reg1::BRF_FBDV_RSTB_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    c.push(cmd::rd(reg::PFDCP_REG));
    c.push(cmd::and(pfdcp_reg::BRF_PFDCP_EN_LV));
    c.push(cmd::wr(reg::PFDCP_REG));

    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::and(fbdv_reg1::BRF_FBDV_EN_LV));
    c.push(cmd::or(fbdv_reg1::BRF_FBDV_MOD_STG_LV + 1));
    c.push(cmd::and(fbdv_reg1::BRF_FBDV_MOD_STG_LV));
    c.push(cmd::or(fbdv_reg1::BRF_SDM_CLK_SEL_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    c.push(cmd::rd(reg::VCO_REG2));
    c.push(cmd::and(vco_reg2::BRF_VCO_ACAL_VL_SEL_LV + 2));
    c.push(cmd::and(vco_reg2::BRF_VCO_ACAL_VH_SEL_LV + 2));
    c.push(cmd::wr(reg::VCO_REG2));

    c.push(cmd::rd(reg::RBB_REG1));
    c.push(cmd::and(rbb_reg1::BRF_EN_LDO_RBB_LV));
    c.push(cmd::wr(reg::RBB_REG1));

    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::and(vco_reg1::BRF_VCO3G_EN_LV));
    c.push(cmd::wr(reg::EDR_CAL_REG1));

    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::and(rf_lodist_reg::BRF_LODISTEDR_EN_LV));
    c.push(cmd::and(rf_lodist_reg::BRF_LO_IARY_EN_LV));
    c.push(cmd::and(rf_lodist_reg::BRF_EN_RFBG_LV));
    c.push(cmd::and(rf_lodist_reg::BRF_EN_VDDPSW_LV));
    c.push(cmd::wr(reg::RF_LODIST_REG));

    c.push(cmd::END);
    c.pad_even();
    c
}

/// Build the BT_TXON command sequence (BR/EDR TX startup).
fn build_bt_txon() -> CmdBuilder {
    let mut c = CmdBuilder::new();

    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::or(rf_lodist_reg::BRF_LO_IARY_EN_LV));
    c.push(cmd::or(rf_lodist_reg::BRF_EN_RFBG_LV));
    c.push(cmd::or(rf_lodist_reg::BRF_EN_VDDPSW_LV));
    c.push(cmd::wr(reg::RF_LODIST_REG));

    c.push(cmd::wait(2));

    c.push(cmd::rd(reg::RBB_REG1));
    c.push(cmd::or(rbb_reg1::BRF_EN_LDO_RBB_LV));
    c.push(cmd::wr(reg::RBB_REG1));

    c.push(cmd::RD_FULCAL);
    c.push(cmd::wr(reg::EDR_CAL_REG1));
    c.push(cmd::wr(reg::ATSTBUF_REG));

    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::or(vco_reg1::BRF_VCO3G_EN_LV));
    c.push(cmd::wr(reg::VCO_REG1));

    c.push(cmd::rd(reg::PFDCP_REG));
    c.push(cmd::or(pfdcp_reg::BRF_PFDCP_EN_LV));
    c.push(cmd::or(pfdcp_reg::BRF_PFDCP_ICP_SET_LV));
    c.push(cmd::and(pfdcp_reg::BRF_PFDCP_ICP_SET_LV + 2));
    c.push(cmd::wr(reg::PFDCP_REG));

    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::or(fbdv_reg1::BRF_FBDV_EN_LV));
    c.push(cmd::and(fbdv_reg1::BRF_FBDV_MOD_STG_LV + 1));
    c.push(cmd::or(fbdv_reg1::BRF_FBDV_MOD_STG_LV));
    c.push(cmd::and(fbdv_reg1::BRF_SDM_CLK_SEL_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::and(fbdv_reg1::BRF_FBDV_RSTB_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    c.push(cmd::rd(reg::VCO_REG2));
    c.push(cmd::or(vco_reg2::BRF_VCO_ACAL_VL_SEL_LV + 2));
    c.push(cmd::or(vco_reg2::BRF_VCO_ACAL_VH_SEL_LV + 2));
    c.push(cmd::wr(reg::VCO_REG2));

    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::or(vco_reg1::BRF_VCO_FLT_EN_LV));
    c.push(cmd::wr(reg::VCO_REG1));

    c.push(cmd::rd(reg::OSLO_REG));
    c.push(cmd::or(oslo_reg::BRF_OSLO_EN_LV));
    c.push(cmd::wr(reg::OSLO_REG));

    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::or(rf_lodist_reg::BRF_LODISTEDR_EN_LV));
    c.push(cmd::wr(reg::RF_LODIST_REG));

    c.push(cmd::rd(reg::TBB_REG));
    c.push(cmd::or(tbb_reg::BRF_EN_TBB_IARRAY_LV));
    c.push(cmd::or(tbb_reg::BRF_EN_LDO_DAC_DVDD_LV));
    c.push(cmd::or(tbb_reg::BRF_EN_LDO_DAC_AVDD_LV));
    c.push(cmd::or(tbb_reg::BRF_EN_DAC_LV));
    c.push(cmd::wr(reg::TBB_REG));

    c.push(cmd::rd(reg::TRF_EDR_REG1));
    c.push(cmd::or(trf_edr_reg1::BRF_TRF_EDR_IARRAY_EN_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG1));

    c.push(cmd::rd(reg::TRF_EDR_REG2));
    c.push(cmd::or(trf_edr_reg2::BRF_TRF_EDR_PA_XFMR_SG_LV));
    c.push(cmd::or(trf_edr_reg2::BRF_TRF_EDR_PACAP_EN_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG2));

    // RD DCCAL
    c.push(cmd::RD_DCCAL1);
    c.push(cmd::wr(reg::IQ_PWR_REG1));
    c.push(cmd::RD_DCCAL2);
    c.push(cmd::wr(reg::IQ_PWR_REG2));

    c.push(cmd::rd(reg::TRF_EDR_REG1));
    c.push(cmd::or(trf_edr_reg1::BRF_TRF_EDR_TMX_PU_LV));
    c.push(cmd::or(trf_edr_reg1::BRF_TRF_EDR_TMXBUF_PU_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG1));

    c.push(cmd::rd(reg::RBB_REG5));
    c.push(cmd::or(rbb_reg5::BRF_EN_IARRAY_LV));
    c.push(cmd::wr(reg::RBB_REG5));

    c.push(cmd::rd(reg::RBB_REG2));
    c.push(cmd::or(rbb_reg2::BRF_EN_RVGA_I_LV));
    c.push(cmd::wr(reg::RBB_REG2));

    c.push(cmd::rd(reg::ADC_REG));
    c.push(cmd::or(adc_reg::BRF_EN_LDO_ADCREF_LV));
    c.push(cmd::or(adc_reg::BRF_EN_LDO_ADC_LV));
    c.push(cmd::or(adc_reg::BRF_EN_ADC_I_LV));
    c.push(cmd::wr(reg::ADC_REG));

    c.push(cmd::wait(8));

    c.push(cmd::rd(reg::TRF_EDR_REG2));
    c.push(cmd::or(trf_edr_reg2::BRF_TRF_EDR_PWRMTR_EN_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG2));

    c.push(cmd::wait(5));

    c.push(cmd::rd(reg::RBB_REG5));
    c.push(cmd::or(rbb_reg5::BRF_RVGA_TX_LPBK_EN_LV));
    c.push(cmd::wr(reg::RBB_REG5));

    c.push(cmd::wait(20));

    c.push(cmd::rd(reg::INCCAL_REG1));
    c.push(cmd::or(inccal_reg1::INCCAL_START));
    c.push(cmd::wr(reg::INCCAL_REG1));
    c.push(cmd::wait(9));

    c.push(cmd::rd(reg::TBB_REG));
    c.push(cmd::or(tbb_reg::BRF_DAC_START_LV));
    c.push(cmd::wr(reg::TBB_REG));

    c.push(cmd::rd(reg::TRF_EDR_REG1));
    c.push(cmd::or(trf_edr_reg1::BRF_TRF_EDR_PA_PU_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG1));

    c.push(cmd::END);
    c.pad_even();
    c
}

/// Build the BT_TXOFF command sequence (BR/EDR TX shutdown).
fn build_bt_txoff() -> CmdBuilder {
    let mut c = CmdBuilder::new();

    c.push(cmd::rd(reg::TRF_EDR_REG1));
    c.push(cmd::rd(reg::TRF_EDR_REG1));
    c.push(cmd::and(trf_edr_reg1::BRF_TRF_EDR_PA_PU_LV));
    c.push(cmd::and(trf_edr_reg1::BRF_TRF_EDR_TMX_PU_LV));
    c.push(cmd::and(trf_edr_reg1::BRF_TRF_EDR_TMXBUF_PU_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG1));

    c.push(cmd::rd(reg::TBB_REG));
    c.push(cmd::and(tbb_reg::BRF_EN_TBB_IARRAY_LV));
    c.push(cmd::and(tbb_reg::BRF_EN_LDO_DAC_DVDD_LV));
    c.push(cmd::and(tbb_reg::BRF_EN_LDO_DAC_AVDD_LV));
    c.push(cmd::and(tbb_reg::BRF_EN_DAC_LV));
    c.push(cmd::and(tbb_reg::BRF_DAC_START_LV));
    c.push(cmd::wr(reg::TBB_REG));

    c.push(cmd::rd(reg::TRF_EDR_REG2));
    c.push(cmd::and(trf_edr_reg2::BRF_TRF_EDR_PA_XFMR_SG_LV));
    c.push(cmd::and(trf_edr_reg2::BRF_TRF_EDR_PACAP_EN_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG2));

    c.push(cmd::rd(reg::RBB_REG5));
    c.push(cmd::and(rbb_reg5::BRF_RVGA_TX_LPBK_EN_LV));
    c.push(cmd::wr(reg::RBB_REG5));

    c.push(cmd::wait(2));

    c.push(cmd::rd(reg::TRF_EDR_REG2));
    c.push(cmd::and(trf_edr_reg2::BRF_TRF_EDR_PWRMTR_EN_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG2));

    c.push(cmd::wait(2));

    c.push(cmd::rd(reg::RBB_REG5));
    c.push(cmd::and(rbb_reg5::BRF_EN_IARRAY_LV));
    c.push(cmd::wr(reg::RBB_REG5));

    c.push(cmd::rd(reg::RBB_REG2));
    c.push(cmd::and(rbb_reg2::BRF_EN_RVGA_I_LV));
    c.push(cmd::wr(reg::RBB_REG2));

    c.push(cmd::rd(reg::ADC_REG));
    c.push(cmd::and(adc_reg::BRF_EN_LDO_ADCREF_LV));
    c.push(cmd::and(adc_reg::BRF_EN_LDO_ADC_LV));
    c.push(cmd::and(adc_reg::BRF_EN_ADC_I_LV));
    c.push(cmd::wr(reg::ADC_REG));

    c.push(cmd::rd(reg::TRF_EDR_REG1));
    c.push(cmd::and(trf_edr_reg1::BRF_TRF_EDR_IARRAY_EN_LV));
    c.push(cmd::wr(reg::TRF_EDR_REG1));

    c.push(cmd::rd(reg::OSLO_REG));
    c.push(cmd::and(oslo_reg::BRF_OSLO_EN_LV));
    c.push(cmd::wr(reg::OSLO_REG));

    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::and(vco_reg1::BRF_VCO3G_EN_LV));
    c.push(cmd::and(vco_reg1::BRF_VCO_FLT_EN_LV));
    c.push(cmd::wr(reg::VCO_REG1));

    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::or(fbdv_reg1::BRF_FBDV_RSTB_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    c.push(cmd::rd(reg::PFDCP_REG));
    c.push(cmd::and(pfdcp_reg::BRF_PFDCP_EN_LV));
    c.push(cmd::and(pfdcp_reg::BRF_PFDCP_ICP_SET_LV));
    c.push(cmd::or(pfdcp_reg::BRF_PFDCP_ICP_SET_LV + 2));
    c.push(cmd::wr(reg::PFDCP_REG));

    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::and(fbdv_reg1::BRF_FBDV_EN_LV));
    c.push(cmd::or(fbdv_reg1::BRF_FBDV_MOD_STG_LV + 1));
    c.push(cmd::and(fbdv_reg1::BRF_FBDV_MOD_STG_LV));
    c.push(cmd::or(fbdv_reg1::BRF_SDM_CLK_SEL_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    c.push(cmd::rd(reg::VCO_REG2));
    c.push(cmd::and(vco_reg2::BRF_VCO_ACAL_VL_SEL_LV + 2));
    c.push(cmd::and(vco_reg2::BRF_VCO_ACAL_VH_SEL_LV + 2));
    c.push(cmd::wr(reg::VCO_REG2));

    c.push(cmd::rd(reg::RBB_REG1));
    c.push(cmd::and(rbb_reg1::BRF_EN_LDO_RBB_LV));
    c.push(cmd::wr(reg::RBB_REG1));

    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::and(vco_reg1::BRF_VCO3G_EN_LV));
    c.push(cmd::wr(reg::EDR_CAL_REG1));

    c.push(cmd::rd(reg::RF_LODIST_REG));
    c.push(cmd::and(rf_lodist_reg::BRF_LODISTEDR_EN_LV));
    c.push(cmd::and(rf_lodist_reg::BRF_LO_IARY_EN_LV));
    c.push(cmd::and(rf_lodist_reg::BRF_EN_RFBG_LV));
    c.push(cmd::and(rf_lodist_reg::BRF_EN_VDDPSW_LV));
    c.push(cmd::wr(reg::RF_LODIST_REG));

    // Redundant commands to fix control change while txoff:
    c.push(cmd::rd(reg::VCO_REG1));
    c.push(cmd::and(vco_reg1::BRF_VCO5G_EN_LV));
    c.push(cmd::and(vco_reg1::BRF_VCO_FLT_EN_LV));
    c.push(cmd::wr(reg::VCO_REG1));

    c.push(cmd::rd(reg::FBDV_REG1));
    c.push(cmd::and(fbdv_reg1::BRF_FBDV_EN_LV));
    c.push(cmd::or(fbdv_reg1::BRF_FBDV_RSTB_LV));
    c.push(cmd::wr(reg::FBDV_REG1));

    c.push(cmd::rd(reg::PFDCP_REG));
    c.push(cmd::and(pfdcp_reg::BRF_PFDCP_EN_LV));
    c.push(cmd::wr(reg::PFDCP_REG));

    c.push(cmd::rd(reg::TRF_REG1));
    c.push(cmd::and(trf_reg1::BRF_PA_BUF_PU_LV));
    c.push(cmd::and(trf_reg1::BRF_TRF_SIG_EN_LV));
    c.push(cmd::and(trf_reg1::BRF_PA_OUT_PU_LV));
    c.push(cmd::wr(reg::TRF_REG1));

    c.push(cmd::END);
    c.pad_even();
    c
}

/// Initialize INCCAL timing registers.
fn init_inccal_timing() {
    // Use write() instead of modify() to clear residual idac_offset/pdx_offset fields.
    BT_RFC.inccal_reg1().write(|w| {
        w.set_vco3g_auto_incacal_en(false);
        w.set_vco3g_auto_incfcal_en(false);
        w.set_vco3g_incacal_wait_time(0x3F);
        w.set_vco3g_incfcal_wait_time(0x3F);
        w.set_vco3g_idac_offset(0);
        w.set_vco3g_pdx_offset(0);
        w.set_frc_inccal_clk_on(false);
    });
    BT_RFC.inccal_reg2().write(|w| {
        w.set_vco5g_auto_incacal_en(false);
        w.set_vco5g_auto_incfcal_en(false);
        w.set_vco5g_incacal_wait_time(0x3F);
        w.set_vco5g_incfcal_wait_time(0x3F);
        w.set_vco5g_idac_offset(0);
        w.set_vco5g_pdx_offset(0);
    });
}

/// Write packed RFC commands to a `RamSlice` at the given byte offset.
fn write_cmd(region: &RamSlice, cmd: &CmdBuilder, offset: u32) -> u32 {
    for i in 0..cmd.packed_word_count() {
        region.write::<u32>((offset as usize) + i * 4, cmd.packed_word(i));
    }
    offset + cmd.byte_len() as u32
}

/// Generate all RFC command sequences and write them to RFC SRAM.
///
/// 1. Initializes INCCAL timing registers
/// 2. Builds 6 command sequences (rxon/rxoff/txon/txoff/bt_txon/bt_txoff)
/// 3. Writes them to RFC SRAM
/// 4. Sets `CU_ADDR_REG1/2/3` to point to the sequences
///
/// Returns the next free SRAM offset after all sequences.
pub fn generate_rfc_cmd_sequences(sram: &RamSlice) -> u32 {
    init_inccal_timing();

    let mut addr: u32 = 0;

    // === RXON ===
    let rxon = build_rxon();
    let rxon_addr = addr;
    BT_RFC.cu_addr_reg1().write(|w| {
        w.set_rxon_cfg_addr(rxon_addr as u16);
    });
    addr = write_cmd(sram, &rxon, rxon_addr);

    // === RXOFF ===
    let rxoff = build_rxoff();
    let rxoff_addr = addr + 4;
    BT_RFC.cu_addr_reg1().modify(|w| {
        w.set_rxoff_cfg_addr(rxoff_addr as u16);
    });
    addr = write_cmd(sram, &rxoff, rxoff_addr);

    // === TXON ===
    let txon = build_txon();
    let txon_addr = addr + 4;
    BT_RFC.cu_addr_reg2().write(|w| {
        w.set_txon_cfg_addr(txon_addr as u16);
    });
    addr = write_cmd(sram, &txon, txon_addr);

    // === TXOFF ===
    let txoff = build_txoff();
    let txoff_addr = addr + 4;
    BT_RFC.cu_addr_reg2().modify(|w| {
        w.set_txoff_cfg_addr(txoff_addr as u16);
    });
    addr = write_cmd(sram, &txoff, txoff_addr);

    // === BT_TXON ===
    let bt_txon = build_bt_txon();
    let bt_txon_addr = addr + 4;
    BT_RFC.cu_addr_reg3().write(|w| {
        w.set_bt_txon_cfg_addr(bt_txon_addr as u16);
    });
    addr = write_cmd(sram, &bt_txon, bt_txon_addr);

    // === BT_TXOFF ===
    let bt_txoff = build_bt_txoff();
    let bt_txoff_addr = addr + 4;
    BT_RFC.cu_addr_reg3().modify(|w| {
        w.set_bt_txoff_cfg_addr(bt_txoff_addr as u16);
    });
    addr = write_cmd(sram, &bt_txoff, bt_txoff_addr);

    addr
}
