use sysmodule_power_api::{ChargerState, ChargerStatus};

pub(crate) struct ChargerCalibration {
    pub bg_prog_v1p2: u8,
    pub cv_vctrl: u8,
    pub cc_mn: u8,
    pub cc_mp: u8,
    pub chg_step: u8,
}

// ── eFuse calibration decode (§7) ───────────────────────────────────────────

pub(crate) fn decode_charger_cal(bank1: &[u8; 32]) -> Option<ChargerCalibration> {
    if bank1[0] == 0 {
        return None;
    }

    let bg_prog_v1p2 = (bank1[10] & 0xF0) >> 4;
    let cv_vctrl = bank1[11] & 0x3F;
    let cc_mn = (bank1[11] >> 6) | ((bank1[12] & 0x07) << 2);
    let cc_mp = bank1[12] >> 3;
    let chg_step = ((bank1[14] & 0xF0) >> 4) | ((bank1[15] & 0x0F) << 4);

    Some(ChargerCalibration {
        bg_prog_v1p2,
        cv_vctrl,
        cc_mn,
        cc_mp,
        chg_step,
    })
}

// ── CC current encoding (§8) ────────────────────────────────────────────────

pub(crate) fn encode_cc_current(ma: u16) -> u8 {
    let (step, base_code, base): (u16, u8, u16) = if ma <= 80 {
        (5, 0x00, 0)
    } else if ma <= 240 {
        (10, 0x10, 80)
    } else {
        (20, 0x20, 240)
    };
    let code = ((ma - base).saturating_sub(1) / step) + base_code as u16;
    (code as u8).min(0x2F)
}

// ── Voltage encoding (§10) ──────────────────────────────────────────────────

fn vbat_step_x4(cal: &ChargerCalibration) -> i32 {
    if cal.chg_step != 0 {
        cal.chg_step as i32
    } else {
        80 // 20.0 mV × 4
    }
}

fn encode_voltage(target_mv: u16, cal: &ChargerCalibration) -> u8 {
    let step_x4 = vbat_step_x4(cal);
    let delta_x4 = (target_mv as i32 - 4200) * 4;
    // round-to-nearest integer division
    let delta_codes = if delta_x4 >= 0 {
        (delta_x4 + step_x4 / 2) / step_x4
    } else {
        (delta_x4 - step_x4 / 2) / step_x4
    };
    (cal.cv_vctrl as i32 + delta_codes).clamp(0, 0x3F) as u8
}

// ── EOC encoding (§11) ──────────────────────────────────────────────────────

fn encode_eoc(percent: u8) -> (u8, bool) {
    let (range_eoc, step, start) = if percent >= 20 {
        (true, 4u8, 8u8)
    } else {
        (false, 2, 4)
    };
    let bm_eoc = percent.saturating_sub(start) / step;
    (bm_eoc.min(7), range_eoc)
}

// ── Reset pulse (§12) ───────────────────────────────────────────────────────

fn reset_pulse(pmuc: sifli_pac::pmuc::Pmuc) {
    pmuc.chg_cr3().modify(|w| w.set_force_rst(true));
    pmuc.chg_cr3().modify(|w| w.set_force_rst(false));
}

// ── Initialization (§13) ────────────────────────────────────────────────────

pub(crate) fn init_charger(pmuc: sifli_pac::pmuc::Pmuc, cal: &ChargerCalibration) {
    // Step 3: write factory trims
    pmuc.chg_cr1().modify(|w| {
        w.set_cc_mn(cal.cc_mn);
        w.set_cc_mp(cal.cc_mp);
        w.set_cv_vctrl(cal.cv_vctrl);
    });
    pmuc.chg_cr2().modify(|w| {
        w.set_bg_prog_v1p2(cal.bg_prog_v1p2);
        w.set_rep_vctrl(cal.cv_vctrl.saturating_sub(7));
    });

    // Step 4: reset pulse to apply trims
    reset_pulse(pmuc);

    // Step 5: enable charger wakeup from hibernate
    pmuc.wer().modify(|w| w.set_chg(true));

    // Step 6: set CV target (4200 mV)
    pmuc.chg_cr1().modify(|w| {
        w.set_cv_vctrl(encode_voltage(4200, cal));
    });
    reset_pulse(pmuc);

    // Set REP threshold (4065 mV) — no reset needed
    pmuc.chg_cr2().modify(|w| {
        w.set_rep_vctrl(encode_voltage(4065, cal));
    });

    // Set VBAT_HIGH threshold (4800 mV) — reset needed
    pmuc.chg_cr2().modify(|w| {
        w.set_high_vctrl(encode_voltage(4800, cal));
    });
    reset_pulse(pmuc);

    // Step 7: set CC current (65 mA)
    pmuc.chg_cr1().modify(|w| {
        w.set_cc_ictrl(encode_cc_current(65));
    });

    // Step 8: set EOC (10% of CC)
    let (bm_eoc, range_eoc) = encode_eoc(10);
    pmuc.chg_cr2().modify(|w| {
        w.set_bm_eoc(bm_eoc);
        w.set_range_eoc(range_eoc);
    });

    // Step 9: enable charger — starts normal Pre-CC/CC/CV/EOC state machine
    // when VBUS is present.  Without this the charger stays in Off/Idle
    // regardless of VBUS, and nothing charges until force_ctrl is asserted
    // by the fob task (which may not start in time on a depleted battery).
    pmuc.chg_cr1().modify(|w| {
        w.set_en(true);
        w.set_loop_en(true);
    });
}

// ── Force-charge mode (§14) ─────────────────────────────────────────────────

pub(crate) fn enter_force_charge(pmuc: sifli_pac::pmuc::Pmuc) {
    pmuc.chg_cr1().modify(|w| {
        w.set_en(true);
        w.set_loop_en(true);
    });
    pmuc.chg_cr3().modify(|w| w.set_force_ctrl(true));
}

pub(crate) fn exit_force_charge(pmuc: sifli_pac::pmuc::Pmuc) {
    pmuc.chg_cr3().modify(|w| w.set_force_ctrl(false));
    pmuc.chg_cr1().modify(|w| {
        w.set_en(false);
        w.set_loop_en(false);
    });
}

// ── Status readback ─────────────────────────────────────────────────────────

pub(crate) fn read_status(pmuc: sifli_pac::pmuc::Pmuc) -> ChargerStatus {
    let sr = pmuc.chg_sr().read();
    let state = match sr.chg_state() {
        0x01 => ChargerState::Off,
        0x02 => ChargerState::PowerUp,
        0x04 => ChargerState::Idle,
        0x08 => ChargerState::PreConstantCurrent,
        0x10 => ChargerState::ConstantCurrent,
        0x20 => ChargerState::ConstantVoltage,
        0x40 => ChargerState::EndOfCharge,
        _ => ChargerState::Unknown,
    };
    ChargerStatus {
        state,
        vbus_present: sr.vbus_rdy_out(),
    }
}
