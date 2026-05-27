#![no_std]
#![no_main]

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
            Ldo::Vdd33Ldo2 => pmuc.peri_ldo().modify(|w| {
                w.set_en_vdd33_ldo2(true);
                w.set_vdd33_ldo2_pd(false);
            }),
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
                    } else {
                        set_led(false);
                        CHARGE_CHANGED.wait().await;
                    }
                }
            }
        ],
    }
}
