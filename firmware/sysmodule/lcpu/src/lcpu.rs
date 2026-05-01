// Low level hardware functions

use core::sync::atomic::{AtomicU8, Ordering};
use sifli_pac::{HPSYS_AON, LPSYS_AON, LPSYS_RCC};

/// LCPU wakeup reference counter (SF32LB52X specific).
/// Used to track nested wakeup requests.
static LCPU_WAKEUP_REF_COUNT: AtomicU8 = AtomicU8::new(0);

const CPU_CYCLES_PER_MICROSECOND: u32 = 240;

// Spin for a number of microseconds
fn blocking_delay_us(usec: u32) {
    cortex_m::asm::delay(CPU_CYCLES_PER_MICROSECOND * usec);
}

/// Reset and halt LCPU. Sets CPUWAIT so LCPU stays halted after reset.
pub fn lcpu_reset_and_halt() {
    if !LPSYS_AON.pmr().read().cpuwait() {
        // halt the lcpu
        LPSYS_AON.pmr().modify(|w| w.set_cpuwait(true));

        // request a reset for the lcpu and the mac
        LPSYS_RCC.rstr1().modify(|w| w.set_lcpu(true));
        LPSYS_RCC.rstr1().modify(|w| w.set_mac(true));

        // spin until neither lcpu reset nor mac reset bits are asserted
        while !LPSYS_RCC.rstr1().read().lcpu() || !LPSYS_RCC.rstr1().read().mac() {}

        // is lcpu sleeping?
        if LPSYS_AON.slp_ctrl().read().sleep_status() {
            // request a wakeup
            LPSYS_AON.slp_ctrl().modify(|w| w.set_wkup_req(true));
            // spin until it wakes up
            while LPSYS_AON.slp_ctrl().read().sleep_status() {}
        }

        // unset lcpu and mac reset (it is unclear why sifli does this)
        LPSYS_RCC.rstr1().modify(|w| w.set_lcpu(false));
        LPSYS_RCC.rstr1().modify(|w| w.set_mac(false));
    }
}

/// Wake up LCPU
///
/// Sends wakeup request to LCPU and waits for acknowledgment.
/// Maintains reference counter for nested calls (SF32LB52X specific).
///
/// # Safety
/// Must be called with HPSYS_AON peripheral access. Caller must ensure
/// paired calls with [`cancel_lcpu_active_request`].
pub unsafe fn wake_lcpu() {
    // Set HP2LP_REQ bit
    HPSYS_AON.issr().modify(|w| w.set_hp2lp_req(true));

    // Wait for LCPU to see the request and respond
    blocking_delay_us(230);
    while !HPSYS_AON.issr().read().lp_active() {}
    blocking_delay_us(30);
    while !HPSYS_AON.issr().read().lp_active() {}

    // Increment reference counter
    LCPU_WAKEUP_REF_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Cancel LCPU active request (paired with wake_lcpu)
///
/// # Safety
/// Must be paired with a prior [`wake_lcpu`] call. Caller must ensure
/// the reference count does not underflow.
pub unsafe fn cancel_lcpu_active_request() {
    let count = LCPU_WAKEUP_REF_COUNT.fetch_sub(1, Ordering::Relaxed);

    // Clear HP2LP_REQ when count reaches 0
    if count == 1 {
        HPSYS_AON.issr().modify(|w| w.set_hp2lp_req(false));
    }
}

/// RAII guard for LCPU wakeup reference count.
/// Calls `cancel_lcpu_active_request()` on drop.
pub struct WakeGuard(());

impl WakeGuard {
    /// Acquire: calls `wake_lcpu()`, incrementing the reference count.
    ///
    /// # Safety
    ///
    /// Caller must ensure that the LCPU subsystem is in a valid state
    /// for wake operations. Multiple concurrent WakeGuards are reference-counted.
    pub unsafe fn acquire() -> Self {
        unsafe {
            wake_lcpu();
        }
        Self(())
    }
}

impl Drop for WakeGuard {
    fn drop(&mut self) {
        unsafe { cancel_lcpu_active_request() };
    }
}

/// BLE initialization orchestration.
///
/// Performs:
/// 1. NVDS write (before LCPU boot)
/// 2. LCPU hardware startup (reset, ROM config, firmware, patches, RF cal)
/// 3. Warmup event consumption + controller init
pub(crate) async fn init_ble<R>(
    lcpu: &Lcpu,
    rev: ChipRevision,
    config: &BleInitConfig,
    dma_ch: impl Peripheral<P = impl Channel>,
    hci_rx: &mut R,
) -> Result<(), BleInitError>
where
    R: embedded_io_async::Read,
{
    // Phase 1: Write NVDS to LCPU shared memory (before boot)
    {
        let _w = unsafe { WakeGuard::acquire() };
        nvds::write_default(&config.ble.bd_addr, config.rom.enable_lxt);
    }

    // Phase 2: LCPU boot sequence
    {
        let _w = unsafe { WakeGuard::acquire() };

        lcpu.reset_and_halt()?;
        rom_config::init(rev, &config.rom, &config.ble.controller);

        // Switch LPSYS sysclk + peri onto HXT48 before LCPU starts running.
        //
        // HAL_PreInit only auto-starts the crystal when its own sysclk is
        // already Hxt48 (`clock_config::init` lines 482-488). A firmware whose
        // boot RccConfig picks DLL1 never takes that branch, so HXT48 stays
        // off and switching LPSYS to it just parks the LCPU on a dead clock
        // line. Explicitly request the crystal before flipping the muxes.
        rcc::ensure_hxt48_ready();
        rcc::select_lpsys_sysclk(rcc::lpsys_vals::Sysclk::Hxt48);
        rcc::select_lpsys_peri(rcc::lpsys_vals::mux::Perisel::Hxt48);

        // Start the cross-core global timer. SDK equivalent:
        // `HAL_HPAON_StartGTimer()` in `bf0_hal_hpaon.c`, called from
        // `HAL_PreInit` on the HCPU and again on the LCPU side of bsp_init.
        //
        // The BLE link-layer scheduler uses GTIMER as its wall-clock; without
        // `CR1.GTIM_EN` on both HPSYS_AON and LPSYS_AON the controller reports
        // success for HCI `Le_Set_Adv_Enable` but never actually triggers a
        // radio event, so the device is invisible on the air.
        use crate::pac::{HPSYS_AON, LPSYS_AON};
        LPSYS_AON.cr1().modify(|w| w.set_gtim_en(true));
        HPSYS_AON.cr1().modify(|w| w.set_gtim_en(true));
        // Sync the two counters. SDK writes 1, hardware latches both sides.
        HPSYS_AON.gtimr().write(|w| w.0 = 1);

        if !config.skip_frequency_check {
            rcc::ensure_safe_lcpu_frequency().map_err(|_| LcpuError::RccError)?;
        }

        if let ChipRevision::A3OrEarlier(_) = rev {
            if let Some(firmware) = config.firmware {
                lcpu.load_firmware(firmware)?;
            } else {
                return Err(LcpuError::FirmwareMissing.into());
            }
        }

        lcpu.set_start_vector_from_image();

        let patch_data = match rev {
            ChipRevision::A3OrEarlier(_) => config.patch_a3,
            _ => config.patch_letter,
        };
        if let Some(data) = patch_data {
            patch::install(rev, data.list, data.bin).map_err(LcpuError::from)?;
        }

        if !config.disable_rf_cal {
            rf_cal::bt_rf_cal(rev, dma_ch);
        }

        lcpu.release()?;
    }

    // Phase 3: Warmup event + controller init
    {
        let _w = unsafe { WakeGuard::acquire() };

        // Consume warmup event
        hci::consume_warmup_event(hci_rx).await?;

        // Controller initialization
        controller::init(rev, &config.ble.controller);
    }

    Ok(())
}
