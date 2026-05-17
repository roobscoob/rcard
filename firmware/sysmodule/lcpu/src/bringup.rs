//! LCPU reset/clock orchestration (phases 2, 4, 5, 8 of the recipe).
//!
//! Phase 2: hold LCPU in reset, clear sleep state if needed, then
//!          deassert reset while CPUWAIT keeps the core stalled so HCPU
//!          can prepare RAM contents.
//! Phase 4: switch LPSYS to the 48 MHz crystal, divide HCLK down to 24 MHz
//!          (LCPU's max rated frequency), sync the global timers.
//! Phase 5: (A3 only) copy firmware blob into LPSYS RAM and load SP/PC
//!          from its vector table into LPSYS_AON.spr/pcr. Letter boots
//!          from internal ROM and skips this.
//! Phase 8: drop CPUWAIT — LCPU starts executing.

use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use sifli_pac::lpsys_rcc::vals::{Hdiv, Sysclk, mux::Perisel};
use sifli_pac::{HPSYS_AON, LPSYS_AON, LPSYS_RCC};

use crate::addr;
use crate::api::LcpuInitError;

/// Outstanding `WakeLock` instances. Bit assertion happens on the
/// 0→1 transition; clear on the 1→0 transition.
static WAKE_REFCOUNT: AtomicU32 = AtomicU32::new(0);

/// Refcounted RAII guard that asserts `HPSYS_AON.ISSR.hp2lp_req` so
/// LCPU stays out of the deep LP sleep state from which LPSYS
/// peripherals (e.g. MAILBOX2) and shared LPSYS_RAM aren't reachable
/// by HCPU. The first outstanding guard asserts and polls `lp_active`
/// for ack; subsequent guards are cheap counter bumps; the last guard
/// to drop clears the bit.
///
/// Modeled on sifli-rs's `WakeLock`. Use it whenever HCPU touches
/// LPSYS-domain registers or shared RAM:
///
/// ```ignore
/// let _wake = WakeLock::new();
/// // ...LPSYS access...
/// // _wake drops at end of scope.
/// ```
///
/// On the 0→1 transition, polls `lp_active` for ~4 ms; if LCPU never
/// acks we still return and emit a warning. Subsequent shared accesses
/// may then fault — surface that explicitly rather than silently spin.
pub struct WakeLock {
    _private: (),
}

impl WakeLock {
    pub fn new() -> Self {
        let prev = WAKE_REFCOUNT.fetch_add(1, Ordering::AcqRel);
        if prev == 0 {
            HPSYS_AON.issr().modify(|w| w.set_hp2lp_req(true));
            let mut budget = 1_000_000u32;
            while !HPSYS_AON.issr().read().lp_active() && budget > 0 {
                budget -= 1;
            }
            if budget == 0 {
                rcard_log::warn!(
                    "LCPU did not ack wake within budget — subsequent shared accesses may fault"
                );
            }
        }
        Self { _private: () }
    }
}

impl Drop for WakeLock {
    fn drop(&mut self) {
        let prev = WAKE_REFCOUNT.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            HPSYS_AON.issr().modify(|w| w.set_hp2lp_req(false));
        }
    }
}

/// Generous polling budget. Each `cortex_m::asm::delay(1)` is one nop;
/// the inner waits clock at 240 MHz so 1_000_000 iterations is ~4 ms.
const POLL_BUDGET: u32 = 10_000_000;

/// A3 LCPU firmware image. Loaded into `LPSYS_RAM_BASE` before phase 8.
/// The vector table's first two words supply SP and PC.
const FIRMWARE_A3: &[u8] = include_bytes!("../data/lcpu_firmware.bin");

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

    // Switch LPSYS sysclk + peripheral clock to HXT48 (48 MHz).
    LPSYS_RCC.csr().modify(|w| w.set_sel_sys(Sysclk::Hxt48));
    LPSYS_RCC.csr().modify(|w| w.set_sel_peri(Perisel::Hxt48));

    // Cap LCPU HCLK at 24 MHz: write HDIV1 = 2 so hclk_lpsys = 48 / 2 = 24 MHz.
    // LCPU is rated for at most 24 MHz; running at 48 MHz (the reset default
    // with sysclk = HXT48) almost certainly causes issues. The chip encodes the
    // divisor directly in the register field, matching sifli-rs's
    // `config_lpsys_hclk_mhz`. The `Hdiv` enum naming is misleading; the
    // raw bit pattern `2` is what produces `/2`.
    LPSYS_RCC.cfgr().modify(|w| w.set_hdiv1(Hdiv::from_bits(2)));

    // Enable both halves of the global timer.
    LPSYS_AON.cr1().modify(|w| w.set_gtim_en(true));
    HPSYS_AON.cr1().modify(|w| w.set_gtim_en(true));
    // Writing 1 to GTIMR latches the cross-core sync.
    HPSYS_AON.gtimr().write(|w| w.set_cnt(1));

    Ok(())
}

/// Copy the A3 firmware blob into `LPSYS_RAM_BASE` and program SP/PC
/// from its vector table (phase 5). Mirrors sifli-rs's
/// `Lcpu::load_firmware` + `set_start_vector_from_image`.
///
/// Returns the (sp, pc) pair for logging. Caller must keep CPUWAIT high
/// while this runs.
pub fn load_a3_firmware() -> Result<(u32, u32), LcpuInitError> {
    if FIRMWARE_A3.len() > addr::A3_LPSYS_RAM_SIZE {
        return Err(LcpuInitError::FirmwareTooLarge);
    }

    // Copy image to LPSYS_RAM_BASE. The first two u32s are SP and the
    // Reset_Handler entry point (with the Thumb bit already set by the
    // linker).
    unsafe {
        ptr::copy_nonoverlapping(
            FIRMWARE_A3.as_ptr(),
            addr::LPSYS_RAM_BASE as *mut u8,
            FIRMWARE_A3.len(),
        );
    }

    // Read SP/PC back out of the image's vector table via the LCPU SRAM
    // alias so the values reflect what LCPU itself will see.
    let sp = unsafe { ptr::read_volatile(addr::LPSYS_RAM_BASE as *const u32) };
    let pc = unsafe { ptr::read_volatile((addr::LPSYS_RAM_BASE + 4) as *const u32) };

    LPSYS_AON.spr().write(|w| w.set_sp(sp));
    LPSYS_AON.pcr().write(|w| w.set_pc(pc));

    Ok((sp, pc))
}

/// Release LCPU from reset (phase 8). After this call LCPU starts
/// executing and will eventually post the warmup HCI event.
pub fn release_lcpu() {
    LPSYS_AON.pmr().modify(|w| w.set_cpuwait(false));
}
