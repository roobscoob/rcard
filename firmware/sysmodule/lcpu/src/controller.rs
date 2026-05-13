//! Post-warmup BLE controller initialization (phase 10 of bringup).
//!
//! Mirrors `sifli-radio/src/bluetooth/controller.rs::init()` minus the
//! RF-cal path and the `pm_init` path (we ship with controller sleep
//! disabled on first cut).
//!
//! Letter writes `BtRomConfig` *pre*-boot inside `rom_config::write_letter`;
//! A3 writes it *post*-boot here (the A3 ROM-config block is only 64 B
//! and doesn't include the BT sub-struct).

use core::ptr;

use sifli_pac::lpsys_rcc::vals::Macdiv;
use sifli_pac::ptc::vals::Op as PtcOp;
use sifli_pac::{LPSYS_AON, LPSYS_RCC, PTC2};
use sysmodule_syscon_api::ChipRev;

use crate::addr;
use crate::bringup;
use crate::rom_config;

/// PTC trigger source for "BLE packet detected" event from LCPU MAC.
const PTC_LCPU_BT_PKTDET: u8 = 105;

/// CFO phase scratch in BT_RFC region. Cleared then OR-latched by PTC2
/// channel 1 on every packet-detect event.
const CFO_PHASE_ADDR: u32 = 0x4008_2790;
const CFO_PHASE_SIZE: usize = 0x0A;

/// Dispatch to the rev-specific post-init routine.
pub fn post_init(rev: ChipRev) {
    match rev {
        ChipRev::Letter => post_init_letter(),
        ChipRev::A3OrEarlier => post_init_a3(),
    }
}

/// Letter: BtRomConfig already in ROM-config block; just do MAC clock,
/// CFO tracking, and close the sleep gate.
///
/// All four operations write to LPSYS peripherals (`LPSYS_RCC`, `PTC2`,
/// `BT_RFC`, `LPSYS_AON`). After warmup, LCPU has dropped into its ROM
/// idle loop and may transition into LP sleep at any point — HCPU
/// accesses to LPSYS while `LP_ACTIVE` is low silently fail. The wake
/// hold is held for the entire body so every write reaches the
/// hardware, and crucially so `LPSYS_AON.reserve0 = 1` actually takes
/// effect before the ROM's next idle-loop iteration reads it.
fn post_init_letter() {
    bringup::hold_lcpu_awake();
    configure_mac_clock();
    setup_cfo_tracking();
    disable_ble_sleep();
    bringup::release_lcpu_hold();
}

/// A3: write `lld_prog_delay` to `RWIP_PROG_DELAY_A3` and the full
/// `BtRomConfig` to `G_ROM_CONFIG_A3`, then run the same common steps
/// as Letter.
///
/// The wake hold spans the full function for the same reason as in
/// `post_init_letter` — every operation here writes into LPSYS, and
/// releasing wake between writes lets LCPU drop into LP sleep mid-init,
/// at which point subsequent writes are lost. In particular, the
/// `disable_ble_sleep` flag write at the end must happen while LCPU is
/// awake so the ROM idle loop observes the new value.
fn post_init_a3() {
    bringup::hold_lcpu_awake();
    // `lld_prog_delay` is a u8 the ROM uses to time low-level radio scheduling.
    unsafe {
        ptr::write_volatile(addr::RWIP_PROG_DELAY_A3 as *mut u8, rom_config::LLD_PROG_DELAY);
    }
    apply_a3_bt_rom_config();
    configure_mac_clock();
    setup_cfo_tracking();
    disable_ble_sleep();
    bringup::release_lcpu_hold();
}

/// Build a `BtRomConfig` matching the Letter pre-boot values and write
/// it to `G_ROM_CONFIG_A3`. Per sifli-rs, OR-merge `bit_valid` with the
/// existing value so we don't clobber any flags the ROM may have set.
fn apply_a3_bt_rom_config() {
    let base = addr::G_ROM_CONFIG_A3;
    let new_valid = rom_config::VALID_CONTROLLER_ENABLE_BIT
        | rom_config::VALID_LLD_PROG_DELAY
        | rom_config::VALID_DEFAULT_SLEEP_MODE
        | rom_config::VALID_DEFAULT_SLEEP_ENABLED
        | rom_config::VALID_DEFAULT_XTAL_ENABLED
        | rom_config::VALID_DEFAULT_RC_CYCLE
        | rom_config::VALID_IS_FPGA;

    unsafe {
        // Per-field writes, matching `BtRomConfig::apply` in sifli-rs.
        ptr::write_volatile(
            (base + rom_config::BT_OFF_CONTROLLER_ENABLE_BIT) as *mut u8,
            0x03, // BLE + BT both on
        );
        ptr::write_volatile(
            (base + rom_config::BT_OFF_LLD_PROG_DELAY) as *mut u8,
            rom_config::LLD_PROG_DELAY,
        );
        ptr::write_volatile(
            (base + rom_config::BT_OFF_DEFAULT_SLEEP_MODE) as *mut u8,
            0,
        );
        ptr::write_volatile(
            (base + rom_config::BT_OFF_DEFAULT_SLEEP_ENABLED) as *mut u8,
            0, // pm_enabled = false
        );
        ptr::write_volatile(
            (base + rom_config::BT_OFF_DEFAULT_XTAL_ENABLED) as *mut u8,
            1, // xtal_enabled = true
        );
        ptr::write_volatile(
            (base + rom_config::BT_OFF_DEFAULT_RC_CYCLE) as *mut u8,
            rom_config::DEFAULT_RC_CYCLE,
        );
        ptr::write_volatile((base + rom_config::BT_OFF_IS_FPGA) as *mut u8, 0);

        // OR-merge bit_valid.
        let bv_ptr = (base + rom_config::BT_OFF_BIT_VALID) as *mut u32;
        let cur = ptr::read_volatile(bv_ptr);
        ptr::write_volatile(bv_ptr, cur | new_valid);
    }
}

/// Configure the BLE MAC baseband clock to 8 MHz.
///
/// `bringup::clock_lcpu_off_hxt48` parks LPSYS on HXT48 (48 MHz) and sets
/// HDIV1 = 2 so LPSYS_HCLK = 24 MHz (LCPU's max rated frequency).
/// Target MAC = 8 MHz → divider = 24 / 8 = 3. The `macfreq` field is
/// informational (a hint to the controller) and matches the SDK's `0x08`.
fn configure_mac_clock() {
    LPSYS_RCC.cfgr().modify(|w| {
        w.set_macdiv(Macdiv::Div3);
        w.set_macfreq(0x08);
    });
}

/// Configure PTC2 channel 1 to latch the CFO phase from the MAC into RAM
/// every time the LCPU sees a BLE packet.
fn setup_cfo_tracking() {
    // PTC2 clock on.
    LPSYS_RCC.enr1().modify(|w| w.set_ptc2(true));

    // Clear the 10-byte scratch.
    unsafe {
        ptr::write_bytes(CFO_PHASE_ADDR as *mut u8, 0, CFO_PHASE_SIZE);
    }

    PTC2.tar1().write(|w| w.set_addr(CFO_PHASE_ADDR));
    PTC2.tdr1().write(|w| w.set_data(0));
    PTC2.tcr1().write(|w| {
        w.set_trigsel(PTC_LCPU_BT_PKTDET);
        w.set_op(PtcOp::Or);
        w.set_trigpol(false);
    });

    PTC2.icr().write(|w| {
        w.set_ctcif1(true);
        w.set_cteif(true);
    });

    PTC2.ier().modify(|w| {
        w.set_tcie1(true);
        w.set_teie(true);
    });
}

/// Close the BLE sleep gate so ROM's `_rwip_sleep` doesn't try to put
/// LCPU into LP sleep. Mirrors sifli-rs's `disable_ble_sleep()`.
fn disable_ble_sleep() {
    LPSYS_AON.reserve0().write(|w| w.set_data(1));
}
