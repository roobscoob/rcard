//! Post-warmup BLE controller initialization (phase 10 of bringup).
//!
//! Mirrors `sifli-radio/src/bluetooth/controller.rs::init()` minus the
//! A3-only sleep-timing patch (we're Letter-only) and the `pm_init`
//! path (we ship with controller sleep disabled on first cut).

use sifli_pac::ptc::vals::Op as PtcOp;
use sifli_pac::lpsys_rcc::vals::Macdiv;
use sifli_pac::{LPSYS_AON, LPSYS_RCC, PTC2};

use core::ptr;

/// PTC trigger source for "BLE packet detected" event from LCPU MAC.
const PTC_LCPU_BT_PKTDET: u8 = 105;

/// CFO phase scratch in BT_RFC region. Cleared then OR-latched by PTC2
/// channel 1 on every packet-detect event.
const CFO_PHASE_ADDR: u32 = 0x4008_2790;
const CFO_PHASE_SIZE: usize = 0x0A;

/// Configure the BLE MAC baseband clock to 8 MHz.
///
/// We've just clocked LPSYS off HXT48 (48 MHz) with HDIV1 left at the
/// default Div1, so LPSYS_HCLK = 48 MHz. Target MAC = 8 MHz → div = 6.
/// The `macfreq` field is informational (a hint to the controller) and
/// matches the SDK's `0x08`.
fn configure_mac_clock() {
    LPSYS_RCC.cfgr().modify(|w| {
        w.set_macdiv(Macdiv::Div6);
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

/// Run all post-warmup steps that apply to first-cut Letter rev.
pub fn post_init() {
    configure_mac_clock();
    setup_cfo_tracking();
    disable_ble_sleep();
}
