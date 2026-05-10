#![no_std]
#![no_main]

use generated::slots::SLOTS;
use sysmodule_syscon_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(Log);

struct SysconImpl;

impl Syscon for SysconImpl {
    fn enable_ldo(_meta: ipc::Meta, ldo: Ldo) -> Result<(), SysconError> {
        rcard_log::info!("enable_ldo: {}", ldo);
        let pmuc = sifli_pac::PMUC;
        match ldo {
            Ldo::Vdd33Ldo3 => pmuc.peri_ldo().modify(|w| w.set_en_vdd33_ldo3(true)),
        }
        rcard_log::info!("enable_ldo: done");
        Ok(())
    }

    fn disable_ldo(_meta: ipc::Meta, ldo: Ldo) -> Result<(), SysconError> {
        rcard_log::info!("disable_ldo: {}", ldo);
        let pmuc = sifli_pac::PMUC;
        match ldo {
            Ldo::Vdd33Ldo3 => pmuc.peri_ldo().modify(|w| w.set_en_vdd33_ldo3(false)),
        }
        Ok(())
    }
}

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("Awake");

    ipc::server! {
        Syscon: SysconImpl,
    }
}
