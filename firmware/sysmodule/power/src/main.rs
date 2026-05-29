#![no_std]
#![no_main]

mod battery;
mod charger;
mod irq;

use core::future::Future as _;
use core::sync::atomic::{AtomicBool, Ordering};

use generated::notifications;
use generated::slots::SLOTS;
use once_cell::OnceCell;
use sysmodule_power_api::*;
use sysmodule_reactor_api::OverflowStrategy;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(Log);

sysmodule_efuse_api::bind_efuse!(Efuse = SLOTS.sysmodule_efuse);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

static CAL: OnceCell<charger::ChargerCalibration> = OnceCell::new();

/// PA24 — LED (green, active-high)
const LED_BIT: u32 = 1 << 24;

static CHARGING: AtomicBool = AtomicBool::new(false);
static CHARGE_CHANGED: ipc::executor::Signal<()> = ipc::executor::Signal::new();
static BATTERY_ADEQUATE: AtomicBool = AtomicBool::new(false);

fn gpio() -> sifli_pac::hpsys_gpio::HpsysGpio {
    sifli_pac::HPSYS_GPIO
}

fn set_led(on: bool) {
    let g = gpio();
    if on {
        g.dosr0().write(|w| w.0 = LED_BIT);
    } else {
        g.docr0().write(|w| w.0 = LED_BIT);
    }
}

struct PowerImpl;

impl Power for PowerImpl {
    fn enable_ldo(_meta: ipc::Meta, ldo: Ldo) -> Result<(), PowerError> {
        rcard_log::info!("enable_ldo: {}", ldo);
        let pmuc = sifli_pac::PMUC;
        match ldo {
            Ldo::Vdd33Ldo2 => {
                if !BATTERY_ADEQUATE.load(Ordering::Relaxed) {
                    rcard_log::warn!("enable_ldo: Vdd33Ldo2 blocked (battery too low)");
                    return Err(PowerError::BatteryTooLow);
                }
                pmuc.peri_ldo().modify(|w| {
                    w.set_en_vdd33_ldo2(true);
                    w.set_vdd33_ldo2_pd(false);
                });
            }
            Ldo::Vdd33Ldo3 => pmuc.peri_ldo().modify(|w| {
                w.set_en_vdd33_ldo3(true);
                w.set_vdd33_ldo3_pd(false);
            }),
        }
        rcard_log::info!("enable_ldo: done");
        Ok(())
    }

    fn disable_ldo(_meta: ipc::Meta, ldo: Ldo) -> Result<(), PowerError> {
        rcard_log::info!("disable_ldo: {}", ldo);
        let pmuc = sifli_pac::PMUC;
        match ldo {
            Ldo::Vdd33Ldo2 => pmuc.peri_ldo().modify(|w| {
                w.set_en_vdd33_ldo2(false);
                w.set_vdd33_ldo2_pd(true);
            }),
            Ldo::Vdd33Ldo3 => pmuc.peri_ldo().modify(|w| {
                w.set_en_vdd33_ldo3(false);
                w.set_vdd33_ldo3_pd(true);
            }),
        }
        Ok(())
    }

    fn charger_status(_meta: ipc::Meta) -> ChargerStatus {
        charger::read_status(sifli_pac::PMUC)
    }

    fn charger_force_start(_meta: ipc::Meta) -> Result<(), PowerError> {
        if CAL.get().is_none() {
            return Err(PowerError::ChargerNotCalibrated);
        }
        charger::enter_force_charge(sifli_pac::PMUC);
        Ok(())
    }

    fn charger_force_stop(_meta: ipc::Meta) -> Result<(), PowerError> {
        charger::exit_force_charge(sifli_pac::PMUC);
        Ok(())
    }
}

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("Awake");

    match Efuse::read(1) {
        Ok(Ok(bank1)) => {
            if let Some(cal) = charger::decode_charger_cal(&bank1) {
                rcard_log::info!("charger: cal loaded, initializing");
                charger::init_charger(sifli_pac::PMUC, &cal);
                irq::configure_charger_irqs(sifli_pac::PMUC);
                let _ = CAL.set(cal);
            } else {
                rcard_log::info!("charger: uncalibrated die, skipping init");
            }
        }
        Ok(Err(_)) | Err(_) => {
            rcard_log::info!("charger: efuse read failed, skipping init");
        }
    }

    // Enable LED output. Start LED off; signal if VBUS is already present
    // so the blink task picks up the initial state on its first poll.
    gpio().doesr0().write(|w| w.0 = LED_BIT);
    set_led(false);
    {
        let status = charger::read_status(sifli_pac::PMUC);
        if status.vbus_present {
            // Charger is already connected: disable the LOWBAT auto-hibernate
            // trigger so a depleted cell doesn't cause an immediate re-hibernate
            // while VBUS is powering the system through the charger rail.
            sifli_pac::PMUC.wer().modify(|w| w.set_lowbat(false));
            // VBUS is present — the charger rail is powering the system, so
            // the display charge pump cannot cause a brownout. Allow Vdd33Ldo2.
            BATTERY_ADEQUATE.store(true, Ordering::Relaxed);
            if CAL.get().is_some() {
                CHARGING.store(true, Ordering::Relaxed);
                CHARGE_CHANGED.signal(());
            }
        } else {
            // Running on battery with no VBUS: lower the AON LDO from 3.3 V
            // (code 6) to 3.0 V (code 0).  If LOWBAT fires and the hardware
            // auto-hibernates, the LDO is already in the low-leakage state
            // that HAL_PMU_EnterHibernate would have set in software.  The
            // kernel restores code 6 on every boot before charger init runs,
            // so charging accuracy is unaffected.
            sifli_pac::PMUC.aon_ldo().modify(|w| w.set_vbat_ldo_set_vout(0));
            // Check SoC before allowing the Vdd33Ldo2 rail up. The display
            // charge pump lives on that rail and causes a brownout loop on a
            // depleted cell. Gate it here, before the async server starts, so
            // the answer is already set when mpr121 first calls enable_ldo.
            match battery::read_vbat_mv_blocking() {
                Some(mv) => {
                    let pct = battery::percent_from_curve(&battery::DISCHARGE_CURVE, mv);
                    rcard_log::info!("battery: {}mV {}% at boot", mv, pct);
                    if pct >= 50 {
                        BATTERY_ADEQUATE.store(true, Ordering::Relaxed);
                    } else {
                        rcard_log::warn!("battery: SoC below 50%, Vdd33Ldo2 blocked");
                    }
                }
                None => {
                    rcard_log::warn!("battery: GPADC read failed at boot, Vdd33Ldo2 blocked");
                }
            }
        }
    }

    ipc::async_server! {
        Power: PowerImpl,
        @irq(pmuc_irq) => || {
            if let Some(event) = irq::handle_charger_irq(sifli_pac::PMUC) {
                match event {
                    ChargerEvent::VbusConnected => {
                        // Restore AON LDO to 3.3 V before the charger engages.
                        sifli_pac::PMUC.aon_ldo().modify(|w| w.set_vbat_ldo_set_vout(6));
                        sifli_pac::PMUC.wer().modify(|w| w.set_lowbat(false));
                        CHARGING.store(true, Ordering::Relaxed);
                        CHARGE_CHANGED.signal(());
                    }
                    ChargerEvent::VbusDisconnected => {
                        // Drop AON LDO to 3.0 V to reduce hibernate leakage.
                        sifli_pac::PMUC.aon_ldo().modify(|w| w.set_vbat_ldo_set_vout(0));
                        sifli_pac::PMUC.wer().modify(|w| w.set_lowbat(true));
                        CHARGING.store(false, Ordering::Relaxed);
                        CHARGE_CHANGED.signal(());
                    }
                    ChargerEvent::ChargingComplete => {
                        CHARGING.store(false, Ordering::Relaxed);
                        CHARGE_CHANGED.signal(());
                    }
                    _ => {}
                }
                let _ = Reactor::refresh(
                    notifications::GROUP_ID_POWER_EVENT,
                    event as u32,
                    15,
                    OverflowStrategy::DropOldest,
                );
            }
        },
        @spawn => [
            async {
                loop {
                    if CHARGING.load(Ordering::Relaxed) {
                        set_led(true);
                        embassy_time::Timer::after_millis(500).await;
                        set_led(false);
                        embassy_time::Timer::after_millis(500).await;
                    } else if !BATTERY_ADEQUATE.load(Ordering::Relaxed) {
                        // Low battery: brief flash every 3s so the device is
                        // visibly alive without being mistaken for charging.
                        set_led(true);
                        embassy_time::Timer::after_millis(100).await;
                        set_led(false);
                        embassy_time::Timer::after_millis(2900).await;
                    } else {
                        set_led(false);
                        CHARGE_CHANGED.wait().await;
                    }
                }
            },
            async {
                let mut calc = battery::BatteryCalculator::new(50, 30, 3, 80, 20);
                loop {
                    embassy_time::Timer::after_millis(30_000).await;
                    if let Some(vbat_mv) = battery::read_vbat_mv().await {
                        let charging = CHARGING.load(Ordering::Relaxed);
                        let pct = calc.update(
                            vbat_mv,
                            charging,
                            &battery::CHARGE_CURVE,
                            &battery::DISCHARGE_CURVE,
                        );
                        rcard_log::info!("battery: {}mV {}% charging={}", vbat_mv, pct, charging);
                    } else {
                        rcard_log::warn!("battery: GPADC read timed out");
                    }
                }
            }
        ],
    }
}
