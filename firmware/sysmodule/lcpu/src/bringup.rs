//! LCPU reset/clock orchestration (phases 2, 4, 8 of the recipe).
//!
//! Phase 2: hold LCPU in reset, clear sleep state if needed, then
//!          deassert reset while CPUWAIT keeps the core stalled so HCPU
//!          can prepare RAM contents.
//! Phase 4: switch LPSYS to the 48 MHz crystal, sync the global timers.
//! Phase 8: drop CPUWAIT — LCPU starts executing from internal ROM.

use sifli_pac::lpsys_rcc::vals::{Sysclk, mux::Perisel};
use sifli_pac::{HPSYS_AON, LPSYS_AON, LPSYS_RCC};

use crate::api::LcpuInitError;

/// Generous polling budget. Each `cortex_m::asm::delay(1)` is one nop;
/// the inner waits clock at 240 MHz so 1_000_000 iterations is ~4 ms.
const POLL_BUDGET: u32 = 10_000_000;

/// Hold LCPU in reset (phase 2). Idempotent — if CPUWAIT is already
/// asserted, this returns without changing state.
pub fn lcpu_reset_and_halt() -> Result<(), LcpuInitError> {
    if LPSYS_AON.pmr().read().cpuwait() {
        return Ok(());
    }

    // Stall LCPU on its next instruction fetch.
    LPSYS_AON.pmr().modify(|w| w.set_cpuwait(true));

    // Assert the LP_LCPU and LP_MAC resets.
    LPSYS_RCC.rstr1().modify(|w| w.set_lcpu(true));
    LPSYS_RCC.rstr1().modify(|w| w.set_mac(true));

    // Wait for the reset bits to land.
    let mut budget = POLL_BUDGET;
    while !LPSYS_RCC.rstr1().read().lcpu() || !LPSYS_RCC.rstr1().read().mac() {
        if budget == 0 {
            return Err(LcpuInitError::ResetTimeout);
        }
        budget -= 1;
    }

    // If LCPU was sleeping, request a wake-up so the next reset deassert
    // takes effect cleanly.
    if LPSYS_AON.slp_ctrl().read().sleep_status() {
        LPSYS_AON.slp_ctrl().modify(|w| w.set_wkup_req(true));
        let mut budget = POLL_BUDGET;
        while LPSYS_AON.slp_ctrl().read().sleep_status() {
            if budget == 0 {
                return Err(LcpuInitError::ResetTimeout);
            }
            budget -= 1;
        }
    }

    // Drop reset — CPUWAIT keeps the core halted at the boot vector.
    LPSYS_RCC.rstr1().modify(|w| w.set_lcpu(false));
    LPSYS_RCC.rstr1().modify(|w| w.set_mac(false));

    Ok(())
}

/// Clock LCPU off the 48 MHz crystal and sync the HCPU/LCPU global
/// timers (phase 4).
pub fn clock_lcpu_off_hxt48() -> Result<(), LcpuInitError> {
    // Make sure HXT48 is up. If not requested yet, request and poll.
    if !HPSYS_AON.acr().read().hxt48_rdy() {
        HPSYS_AON.acr().modify(|w| w.set_hxt48_req(true));
        let mut budget = POLL_BUDGET;
        while !HPSYS_AON.acr().read().hxt48_rdy() {
            if budget == 0 {
                return Err(LcpuInitError::Hxt48Timeout);
            }
            budget -= 1;
        }
    }
    if !LPSYS_AON.acr().read().hxt48_rdy() {
        LPSYS_AON.acr().modify(|w| w.set_hxt48_req(true));
        let mut budget = POLL_BUDGET;
        while !LPSYS_AON.acr().read().hxt48_rdy() {
            if budget == 0 {
                return Err(LcpuInitError::Hxt48Timeout);
            }
            budget -= 1;
        }
    }

    // Switch LPSYS sysclk + peripheral clock to HXT48.
    LPSYS_RCC.csr().modify(|w| w.set_sel_sys(Sysclk::Hxt48));
    LPSYS_RCC.csr().modify(|w| w.set_sel_peri(Perisel::Hxt48));

    // Enable both halves of the global timer.
    LPSYS_AON.cr1().modify(|w| w.set_gtim_en(true));
    HPSYS_AON.cr1().modify(|w| w.set_gtim_en(true));
    // Writing 1 to GTIMR latches the cross-core sync.
    HPSYS_AON.gtimr().write(|w| w.set_cnt(1));

    Ok(())
}

/// Release LCPU from reset (phase 8). After this call LCPU starts
/// executing internal ROM and will eventually post the warmup HCI event.
pub fn release_lcpu() {
    LPSYS_AON.pmr().modify(|w| w.set_cpuwait(false));
}
