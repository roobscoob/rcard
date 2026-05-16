#![no_std]
#![no_main]

mod charger;
mod irq;

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

    ipc::server! {
        Power: PowerImpl,
        @irq(pmuc_irq) => || {
            if let Some(event) = irq::handle_charger_irq(sifli_pac::PMUC) {
                let _ = Reactor::refresh(
                    notifications::GROUP_ID_POWER_EVENT,
                    event as u32,
                    15,
                    OverflowStrategy::DropOldest,
                );
            }
        },
    }
}
