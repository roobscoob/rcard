#![no_std]
#![no_main]

use generated::slots::SLOTS;
use rcard_log::{error, info, warn};
use sysmodule_lcpu_api::*;
use sysmodule_device_api::ChipRev;
use sysmodule_reactor_api::OverflowStrategy;

mod addr;
mod bringup;
mod circular_buf;
mod controller;
mod delay;
mod dma;
mod mailbox;
mod nvds;
mod patch;
mod ram_slice;
mod rf_cal;
mod rom_config;

// Re-export the api module so submodules can refer to error types via
// `crate::api::LcpuInitError`.
mod api {
    pub use sysmodule_lcpu_api::*;
}

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);
sysmodule_device_api::bind_device!(Device = SLOTS.sysmodule_device);
sysmodule_efuse_api::bind_efuse!(Efuse = SLOTS.sysmodule_efuse);

/// Stack scratch for moving lease bytes through the mailbox.
const SCRATCH_LEN: usize = 512;

/// Iteration cap for the warmup MISR poll. `WARMUP_POLL_DELAY_CYCLES` of
/// roughly 100 µs each → ~250 ms total budget; the LCPU usually emits
/// the warmup HCI event in a few ms, so this is wildly generous.
const WARMUP_MAX_POLLS: u32 = 2500;
/// Per-iteration delay for the warmup MISR poll. ~100 µs at 240 MHz.
const WARMUP_POLL_DELAY_CYCLES: u32 = 24_000;

/// HCI qid (queue ID) used by both mailbox channels. Bit 0 in the
/// `Ixr` registers. Mirrors `mailbox::HCI_QID` so we can poll MISR here
/// without making it `pub`.
const HCI_QID_BIT: u32 = 1u32 << 0;

struct LcpuResource;

impl Lcpu for LcpuResource {
    fn init(_meta: ipc::Meta, bd_addr: [u8; 6]) -> Result<Self, LcpuInitError> {
        info!("bringup starting");

        let chip_id = match Device::chip_id() {
            Ok(id) => id,
            Err(e) => {
                error!("device chip_id IPC failed: {}", e);
                return Err(LcpuInitError::UnsupportedChipRevision);
            }
        };
        let rev = match chip_id.rev() {
            Some(r) => r,
            None => {
                error!("unsupported chip revid: {}", chip_id.revid);
                return Err(LcpuInitError::UnsupportedChipRevision);
            }
        };
        info!("chip rev: {} (revid={})", rev, chip_id.revid);

        // Read the factory Bank1 calibration once via IPC so rf_cal
        // (phase 7) can apply EDR PA BM adjustments. None on IPC
        // failure or `edr_cal_done` clear — rf_cal degrades gracefully.
        let efuse_cal = match Efuse::read_calibration() {
            Ok(Ok(cal)) => Some(cal),
            Ok(Err(e)) => {
                warn!("efuse read_calibration domain err: {} — skipping", e);
                None
            }
            Err(e) => {
                warn!("efuse read_calibration ipc err: {} — skipping", e);
                None
            }
        };

        match do_bringup(rev, bd_addr, efuse_cal.as_ref()) {
            Ok(()) => {}
            Err(e) => {
                error!("bringup failed: {}", e);
                bringup_teardown();
                return Err(e);
            }
        }

        info!("ready");
        Ok(LcpuResource)
    }

    fn send_hci(
        &mut self,
        _meta: ipc::Meta,
        data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<(), HciSendError> {
        info!("sending HCI...");

        let total = data.len();
        if total == 0 {
            info!("okay! (len==0)");
            return Ok(());
        }
        if total > SCRATCH_LEN {
            info!("sad! (len>=512)");
            return Err(HciSendError::TooLarge);
        }

        let mut local = [0u8; SCRATCH_LEN];
        let n = data.read_range(0, &mut local[..total]).unwrap_or(0);
        if n != total {
            info!("sad! (lease read failed)");
            // Partial read of the lease — treat as the lease being
            // shorter than its declared length.
            return Err(HciSendError::TooLarge);
        }

        // FIXME: edit this when logging supports &[T]
        info!("SENDING: {}", local[..total]);

        let written = mailbox::write_hci(&local[..total]);
        if written != total {
            info!(
                "sad! (hci write failed, written={} total={})",
                written, total
            );
            return Err(HciSendError::TooLarge);
        }
        info!("okay! (did everything)");
        Ok(())
    }

    fn recv_hci(
        &mut self,
        _meta: ipc::Meta,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> u16 {
        info!("recv HCI...");
        let want = core::cmp::min(buf.len(), SCRATCH_LEN);
        if want == 0 {
            info!("recv none! (want==0)");
            return 0;
        }
        let mut local = [0u8; SCRATCH_LEN];
        let n = mailbox::read_hci(&mut local[..want]);
        if n == 0 {
            info!("recv none! (mailbox empty)");
            return 0;
        }
        let the_bytes_in_question = &local[..n];
        let _ = buf.write_range(0, the_bytes_in_question);
        info!("recv ok! (got {} bytes)", the_bytes_in_question.len());
        // FIXME: edit this when logging supports &[T]
        info!("RECEIVED: {}", *the_bytes_in_question);
        n as u16
    }
}

impl Drop for LcpuResource {
    fn drop(&mut self) {
        // Put LCPU back in reset. Errors here are unrecoverable but we
        // can't propagate them through Drop — log and continue.
        //
        // No explicit wake-hold cleanup: `WakeLock` is refcounted RAII,
        // so every acquire in this task is paired with a Drop. If the
        // refcount is nonzero here, something leaked a guard — that's a
        // bug to find, not paper over.
        if let Err(e) = bringup::lcpu_reset_and_halt() {
            warn!("Drop reset_and_halt failed: {}", e);
        }
        info!("released");
    }
}

/// Run phases 1, 2, 3, 4, [5,] 6, 7, 8, 9, 10 of the recipe.
///
/// - Phase 5 (A3 firmware load) runs on A3 only; Letter boots from
///   internal ROM and skips it.
/// - Phase 7 (RF calibration) runs unconditionally and uses the
///   factory `efuse_cal` for EDR power adjustments when available.
fn do_bringup(
    rev: ChipRev,
    bd_addr: [u8; 6],
    efuse_cal: Option<&sysmodule_efuse_api::Bank1Calibration>,
) -> Result<(), LcpuInitError> {
    // Phase 2 first — we need LCPU halted before we mutate its RAM.
    bringup::lcpu_reset_and_halt()?;
    info!("phase 2 reset/halt done");

    // Phase 1 — NVDS TLV blob.
    nvds::write_default(&bd_addr, /*use_lxt=*/ true);
    info!("phase 1 NVDS done");

    // Initialize the HCPU TX ring header before phase 3 so we have its
    // resolved address to write into ROM-config field +200 (Letter) or
    // post-boot G_ROM_CONFIG_A3 (A3 — controller::post_init reads it
    // back from mailbox::tx_ring_hcpu_addr() if needed). Also picks the
    // rev-correct RX ring address.
    mailbox::init_tx_ring(rev);
    let hcpu_ipc_addr = mailbox::tx_ring_hcpu_addr();
    info!(
        "HCPU TX ring at {}, RX ring at {}",
        hcpu_ipc_addr,
        mailbox::rx_ring_hcpu_addr()
    );

    // Phase 3 — ROM-config block.
    rom_config::write(rev, hcpu_ipc_addr);
    info!("phase 3 ROM config done");

    // Phase 4 — clock LCPU off HXT48 + sync gtim.
    bringup::clock_lcpu_off_hxt48()?;
    info!("phase 4 clock done");

    // Phase 5 (A3 only) — load firmware blob to LPSYS RAM and program
    // SP/PC. Letter boots from internal ROM and skips this.
    if matches!(rev, ChipRev::A3OrEarlier) {
        let (sp, pc) = bringup::load_a3_firmware()?;
        info!("phase 5 firmware loaded; SP={} PC={}", sp, pc);
    }

    // Phase 6 — install patches.
    if let Err(e) = patch::install(rev) {
        error!("patch install failed: {}", e);
        return Err(LcpuInitError::PatchInstallFailed);
    }
    info!("phase 6 patches done");

    // Phase 7 — RF calibration. Sets up the BLE controller's RFC
    // command sequences, calibrates VCO + TX DC offset, and stores the
    // resulting tables back into RFC SRAM. Without this, the BLE ROM
    // observes "no radio scheduled" after warmup and drops the LP
    // domain into deep sleep, from which MAILBOX1 IRQs are not a wake
    // source — see plan file for the deep-sleep theory.
    let mut dma_ch = dma::DmacChannel::claim_default();
    rf_cal::bt_rf_cal(rev, &mut dma_ch, efuse_cal);
    info!("phase 7 RF calibration done");

    // Unmask qid 0 on both mailboxes before LCPU starts running so the
    // warmup HCI event isn't dropped on the floor. The kernel-side IRQ
    // enable is owned by the `ipc::server!` macro's `@irq` prelude
    // (already run before init() was dispatched), so we don't touch it
    // here — `wait_for_warmup` polls MISR directly instead.
    mailbox::unmask_hci_qid();

    // Phase 8 — release LCPU.
    bringup::release_lcpu();

    // Give LCPU a moment to start, then sample state.
    // ~1M cycles @ 240 MHz ≈ 4 ms.
    cortex_m::asm::delay(1_000_000);
    {
        let aon = sifli_pac::LPSYS_AON;
        let pmr = aon.pmr().read();
        let slp = aon.slp_ctrl().read();
        let issr = aon.issr().read();
        info!(
            "post-release: CPUWAIT={} SLEEP_STATUS={} LP_ACTIVE={}",
            pmr.cpuwait() as u8,
            slp.sleep_status() as u8,
            issr.lp_active() as u8,
        );
    }
    info!("phase 8 LCPU released; waiting for warmup");

    // Phase 9 — wait for the warmup HCI event. We block on the IRQ
    // notification, drain the RX ring on each wake, and stop once we've
    // consumed at least three bytes whose first byte is 0x04 (HCI Event
    // packet indicator).
    wait_for_warmup()?;
    info!("phase 9 warmup HCI event received");

    // Phase 10 — post-init.
    controller::post_init(rev);
    info!("phase 10 post-init done");

    Ok(())
}

/// Poll `MAILBOX2.misr(0)` qid 0 until LCPU emits the warmup HCI event,
/// then ack, validate the 3-byte H4 header, drain any params, and return.
///
/// We deliberately do **not** go through `sys_recv_notification` /
/// `sys_enable_irq` — the kernel IRQ state for `lpsys_mailbox2_ch1` is
/// owned by the `ipc::server!` macro's `@irq` arm (enabled in its
/// prelude, re-enabled after every closure run). Touching it from
/// inside an IPC dispatch leaves the @irq closure's view of the bit
/// inconsistent across the init() return boundary. Spinning on the
/// hardware register sidesteps that entirely.
fn wait_for_warmup() -> Result<(), LcpuInitError> {
    // Hold LCPU awake for the whole function — MAILBOX2 + LPSYS-RAM
    // access spans every step. `ack_mailbox2_irq` and `read_hci` each
    // take their own `WakeLock`, but those are cheap refcount bumps
    // while this outer guard is live. RAII drops it on every return
    // path.
    let _wake = bringup::WakeLock::new();
    let mut polls = 0u32;
    loop {
        let misr = sifli_pac::MAILBOX2.misr(0).read().0;
        if misr & HCI_QID_BIT != 0 {
            break;
        }
        if polls >= WARMUP_MAX_POLLS {
            return Err(LcpuInitError::WarmupTimeout);
        }
        polls += 1;
        cortex_m::asm::delay(WARMUP_POLL_DELAY_CYCLES);
    }

    // Clear the MAILBOX2 IRQ status now that we've observed it. The
    // ipc::server! @irq closure handles subsequent IRQs after init()
    // returns and the server loop resumes.
    mailbox::ack_mailbox2_irq();

    // Read the 3-byte H4 header.
    let mut header = [0u8; 3];
    let mut header_filled = 0usize;
    for _ in 0..16 {
        let n = mailbox::read_hci(&mut header[header_filled..]);
        header_filled += n;
        if header_filled >= 3 {
            break;
        }
        cortex_m::asm::delay(WARMUP_POLL_DELAY_CYCLES);
    }
    if header_filled < 3 {
        return Err(LcpuInitError::WarmupTimeout);
    }

    info!(
        "warmup H4 header: type={} code={} param_len={}",
        header[0], header[1], header[2]
    );

    if header[0] != 0x04 {
        warn!("warmup frame had bad H4 type {}, expected 0x04", header[0]);
        return Err(LcpuInitError::WarmupBadFrame);
    }

    // Drain any parameter bytes the event carries so the ring is empty
    // before init() returns. The holder's first recv_hci can observe
    // later events without leftovers from warmup.
    let param_len = header[2] as usize;
    let mut params = [0u8; 256];
    let cap = core::cmp::min(param_len, params.len());
    let mut got = 0usize;
    if param_len > 0 {
        for _ in 0..16 {
            let n = mailbox::read_hci(&mut params[got..cap]);
            got += n;
            if got >= cap {
                break;
            }
            cortex_m::asm::delay(WARMUP_POLL_DELAY_CYCLES);
        }
        if got < cap {
            warn!("warmup params short-read: got {} of {} bytes", got, cap);
        }
    }
    // FIXME: edit this when logging supports &[T]
    info!("warmup params: {}", params[..got]);

    Ok(())
}

/// Best-effort cleanup after a failed bringup. Mirrors the `Drop` body
/// minus the holder/atomic clears (init() does those after).
///
/// No explicit wake-hold cleanup: `WakeLock` guards drop with the stack
/// frames they live in, so by the time we get here the refcount should
/// already be 0.
fn bringup_teardown() {
    if let Err(e) = bringup::lcpu_reset_and_halt() {
        warn!("teardown reset_and_halt failed: {}", e);
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    info!("starting");
    ipc::server! {
        Lcpu: LcpuResource,
        @irq(lpsys_mailbox2_ch1) => || {
            info!("IRQ! MAILBOX2_CH1");
            // Drain MISR / clear ICR.
            mailbox::ack_mailbox2_irq();
            // Post to reactor
            match Reactor::refresh(generated::notifications::GROUP_ID_LCPU_DATA, 0, 50, OverflowStrategy::Reject) {
                Ok(Ok(())) => info!("reactor refresh ok"),
                Ok(Err(e)) => warn!("reactor refresh rejected: {}", e),
                Err(e) => warn!("reactor refresh ipc err: {}", e),
            }
        },
    }
}
