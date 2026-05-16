use sysmodule_power_api::ChargerEvent;

// ── IRQ trigger modes (§6) ──────────────────────────────────────────────────

const MODE_POS_EDGE: u8 = 2;
const MODE_DOUBLE_EDGE: u8 = 4;

// ── Configure charger IRQs (§13 step 9) ─────────────────────────────────────

pub(crate) fn configure_charger_irqs(pmuc: sifli_pac::pmuc::Pmuc) {
    // Disable all events before reconfiguring trigger modes
    pmuc.chg_cr4().modify(|w| {
        w.set_ie_vbus_rdy(false);
        w.set_ie_vbat_high(false);
        w.set_ie_above_rep(false);
        w.set_ie_above_cc(false);
        w.set_ie_cc_mode(false);
        w.set_ie_cv_mode(false);
        w.set_ie_eoc_mode(false);
        w.set_ie_eoc(false);
    });

    // Set trigger modes
    pmuc.chg_cr4().modify(|w| {
        w.set_im_vbus_rdy(MODE_DOUBLE_EDGE);
        w.set_im_vbat_high(MODE_POS_EDGE);
        w.set_im_eoc_mode(MODE_POS_EDGE);
    });

    // Clear any pending IRQs (W1C: write to IC bits at [7:0])
    let pending = (pmuc.chg_cr5().read().0 >> 16) as u8;
    if pending != 0 {
        pmuc.chg_cr5().write(|w| w.0 = pending as u32);
    }

    // Enable the events we care about
    pmuc.chg_cr4().modify(|w| {
        w.set_ie_vbus_rdy(true);
        w.set_ie_vbat_high(true);
        w.set_ie_eoc(true);
        w.set_ie_eoc_mode(true);
    });
}

// ── IRQ handler (§15) ───────────────────────────────────────────────────────

pub(crate) fn handle_charger_irq(pmuc: sifli_pac::pmuc::Pmuc) -> Option<ChargerEvent> {
    let status = (pmuc.chg_cr5().read().0 >> 16) as u8;

    if status == 0 {
        return None;
    }

    // W1C: clear all pending bits at once
    pmuc.chg_cr5().write(|w| w.0 = status as u32);

    // Dispatch in priority order: VBUS > overvoltage > EOC > fallback

    if status & (1 << 0) != 0 {
        let vbus_present = pmuc.chg_sr().read().vbus_rdy_out();
        return Some(if vbus_present {
            ChargerEvent::VbusConnected
        } else {
            ChargerEvent::VbusDisconnected
        });
    }

    if status & (1 << 1) != 0 {
        return Some(ChargerEvent::BatteryHigh);
    }

    // EOC: bits 6 (EOC_MODE) and 7 (EOC latched) — handle §4.5 quirk
    if status & ((1 << 6) | (1 << 7)) != 0 {
        return Some(ChargerEvent::ChargingComplete);
    }

    if status & (1 << 4) != 0 {
        return Some(ChargerEvent::ChargingStarted);
    }

    Some(ChargerEvent::StateChanged)
}
