#![no_std]
#![no_main]

use generated::slots::SLOTS;
use rcard_log::{error, info};
use sysmodule_drv2603_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(Log);
sysmodule_clocks_api::bind_clocks!(Clocks = SLOTS.sysmodule_clocks);

/// PA07 — HAPTIC_EN (active-high enable for DRV2603)
const HAPTIC_EN_BIT: u32 = 1 << 7;

fn tim() -> sifli_pac::gptim::Gptim {
    sifli_pac::GPTIM2
}

fn gpio() -> sifli_pac::hpsys_gpio::HpsysGpio {
    sifli_pac::HPSYS_GPIO
}

fn set_haptic_en(on: bool) {
    let g = gpio();
    if on {
        g.dosr0().write(|w| w.0 = HAPTIC_EN_BIT);
    } else {
        g.docr0().write(|w| w.0 = HAPTIC_EN_BIT);
    }
}

fn wait_ms(ms: u64) {
    let start = userlib::sys_get_timer().now;
    while userlib::sys_get_timer().now - start < ms {
        core::hint::spin_loop();
    }
}

fn pwm_init() {
    // Enable and reset GPTIM2 via the clocks sysmodule
    if let Err(e) = Clocks::enable(sysmodule_clocks_api::Peripheral::Gptim2) {
        error!("failed to enable GPTIM2 clock: {:?}", e);
    }
    if let Err(e) = Clocks::reset(sysmodule_clocks_api::Peripheral::Gptim2) {
        error!("failed to reset GPTIM2: {:?}", e);
    }

    let t = tim();

    // 24 MHz peri clock / (PSC+1) / (ARR+1) = 24M / 1 / 1200 = 20 kHz
    t.psc().write(|w| w.set_psc(0));
    t.arr().write(|w| w.set_arr(1199));
    t.ccr(0).write(|w| w.set_ccr(0));

    // PWM mode 1 on CH1, preload enable
    t.ccmr1().write(|w| {
        w.set_ocm(0, 0b0110);
        w.set_ocpe(0, true);
    });

    // Enable CH1 output, active high
    t.ccer().write(|w| w.set_cce(0, true));

    // Auto-reload preload + counter enable
    t.cr1().write(|w| {
        w.set_arpe(true);
        w.set_cen(true);
    });

    // Force update to latch shadow registers
    t.egr().write(|w| w.set_ug(true));

    // Enable HAPTIC_EN as GPIO output, start low
    gpio().doesr0().write(|w| w.0 = HAPTIC_EN_BIT);
    set_haptic_en(false);
}

struct Drv2603Impl;

/// Max drive is 91% of the 50%–100% range → 50%–95.5% duty.
/// CCR 600 = 50% duty (DRV2603 "off"), CCR 1146 = 95.5% (max safe drive).
const CCR_BASE: u16 = 600;
const CCR_RANGE: f32 = 546.0; // 1146 - 600

impl Drv2603 for Drv2603Impl {
    fn drive(_meta: ipc::Meta, strength: f32) -> Result<(), Drv2603Error> {
        let s = strength.clamp(0.0, 1.0);
        let ccr = CCR_BASE + (s * CCR_RANGE) as u16;
        tim().ccr(0).write(|w| w.set_ccr(ccr));
        set_haptic_en(true);
        Ok(())
    }

    fn stop(_meta: ipc::Meta) -> Result<(), Drv2603Error> {
        // Drop CCR below 50% so the DRV2603's internal brake engages,
        // wait for the motor to stop, then cut power.
        tim().ccr(0).write(|w| w.set_ccr(0));
        wait_ms(2);
        set_haptic_en(false);
        Ok(())
    }
}

#[export_name = "main"]
fn main() -> ! {
    info!("drv2603: starting");
    pwm_init();
    info!("drv2603: pwm initialized, entering server loop");

    ipc::server! {
        Drv2603: Drv2603Impl,
    }
}
