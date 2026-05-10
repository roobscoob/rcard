#![no_std]
#![no_main]

use generated::slots::SLOTS;
use rcard_log::{error, info, warn};
use sysmodule_lcpu_api::*;
use sysmodule_reactor_api::OverflowStrategy;

mod addr;
mod bringup;
mod circular_buf;
mod controller;
mod mailbox;
mod nvds;
mod patch;
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

/// Notification bit the kernel posts when MAILBOX2_CH1 IRQ fires for
/// this task. Resolved at codegen time from `mailbox2.irqs.ch1` in the
/// chip ncl + `uses_peripherals` in our task ncl.
const MAILBOX2_BIT: u32 = generated::irq_bit!(sysmodule_lcpu, lpsys_mailbox2_ch1);

/// Stack scratch for moving lease bytes through the mailbox. 256 B is
/// the IPC message size limit so any one lease/reply fits in one pass.
const SCRATCH_LEN: usize = 256;

/// Iteration cap for the warmup wait. Each iteration is one
/// `sys_recv_open` → drain → check cycle. The LCPU usually emits the
/// warmup HCI event in a few ms, so this is wildly generous.
const WARMUP_MAX_WAKES: u32 = 100;

struct LcpuResource;

impl Lcpu for LcpuResource {
    fn init(_meta: ipc::Meta, bd_addr: [u8; 6]) -> Result<Self, LcpuInitError> {
        info!("bringup starting");

        match do_bringup(bd_addr) {
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
        let total = data.len();
        if total == 0 {
            return Ok(());
        }
        if total > SCRATCH_LEN {
            // The IPC layer caps a single message at 256 B, but be
            // explicit so the error path is unambiguous.
            return Err(HciSendError::TooLarge);
        }

        let mut local = [0u8; SCRATCH_LEN];
        let n = data.read_range(0, &mut local[..total]).unwrap_or(0);
        if n != total {
            // Partial read of the lease — treat as the lease being
            // shorter than its declared length.
            return Err(HciSendError::TooLarge);
        }

        let written = mailbox::write_hci(&local[..total]);
        if written != total {
            return Err(HciSendError::TooLarge);
        }
        Ok(())
    }

    fn recv_hci(
        &mut self,
        _meta: ipc::Meta,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> u16 {
        let want = core::cmp::min(buf.len(), SCRATCH_LEN);
        if want == 0 {
            return 0;
        }
        let mut local = [0u8; SCRATCH_LEN];
        let n = mailbox::read_hci(&mut local[..want]);
        if n == 0 {
            return 0;
        }
        let _ = buf.write_range(0, &local[..n]);
        n as u16
    }
}

impl Drop for LcpuResource {
    fn drop(&mut self) {
        // Put LCPU back in reset. Errors here are unrecoverable but we
        // can't propagate them through Drop — log and continue.
        if let Err(e) = bringup::lcpu_reset_and_halt() {
            warn!("Drop reset_and_halt failed: {:?}", e);
        }
        info!("released");
    }
}

/// Run phases 1, 2, 3, 4, 6, 8, 9, 10 of the recipe. Phases 5 (A3 fw
/// load) and 7 (RF cal) are intentionally skipped on first cut.
fn do_bringup(bd_addr: [u8; 6]) -> Result<(), LcpuInitError> {
    // Phase 2 first — we need LCPU halted before we mutate its RAM.
    bringup::lcpu_reset_and_halt()?;
    info!("phase 2 reset/halt done");

    // Phase 1 — NVDS TLV blob.
    nvds::write_default(&bd_addr, /*use_lxt=*/ true);
    info!("phase 1 NVDS done");

    // Initialize the HCPU TX ring header before phase 3 so we have its
    // resolved address to write into ROM-config field +200.
    mailbox::init_tx_ring();
    let hcpu_ipc_addr = mailbox::tx_ring_hcpu_addr();
    info!("HCPU TX ring at {}", hcpu_ipc_addr);

    // Phase 3 — Letter-rev ROM config block.
    rom_config::write_letter(hcpu_ipc_addr);
    info!("phase 3 ROM config done");

    // Phase 4 — clock LCPU off HXT48 + sync gtim.
    bringup::clock_lcpu_off_hxt48()?;
    info!("phase 4 clock done");

    // Phase 6 — install patches.
    if let Err(e) = patch::install_letter() {
        error!("patch install failed: {}", e);
        return Err(LcpuInitError::PatchInstallFailed);
    }
    info!("phase 6 patches done");

    // Unmask qid 0 on both mailboxes before LCPU starts running so the
    // warmup HCI event isn't dropped on the floor.
    mailbox::unmask_hci_qid();
    userlib::sys_enable_irq_and_clear_pending(MAILBOX2_BIT);

    // Phase 8 — release LCPU.
    bringup::release_lcpu();

    // Give LCPU a moment to start its ROM, then sample state.
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

    // Poll for ~200 ms to see if LCPU writes anything. We dump
    // MAILBOX2.MISR (raises the IRQ when non-zero) and the LCPU→HCPU
    // CircularBuf header so we can tell whether LCPU has posted bytes
    // and/or rung the doorbell.
    for tick in 0..10u32 {
        cortex_m::asm::delay(5_000_000); // ~20 ms each pass
        let misr = sifli_pac::MAILBOX2.misr(0).read().0;
        let cb = addr::LCPU2HCPU_MB_CH1 as *const circular_buf::CircularBuf;
        let (rd_ptr, wr_ptr, size, rd_idx, wr_idx) = unsafe {
            (
                core::ptr::read_volatile(core::ptr::addr_of!((*cb).rd_buffer_ptr)) as u32,
                core::ptr::read_volatile(core::ptr::addr_of!((*cb).wr_buffer_ptr)) as u32,
                core::ptr::read_volatile(core::ptr::addr_of!((*cb).buffer_size)) as i32,
                core::ptr::read_volatile(core::ptr::addr_of!((*cb).read_idx_mirror)),
                core::ptr::read_volatile(core::ptr::addr_of!((*cb).write_idx_mirror)),
            )
        };
        info!(
            "tick {}: MISR={} rx_cb size={} rd_ptr={} wr_ptr={} rd_idx_mirror={} wr_idx_mirror={}",
            tick, misr, size, rd_ptr, wr_ptr, rd_idx, wr_idx,
        );
        if misr != 0 || wr_idx != 0 {
            break;
        }
    }
    info!("phase 8 LCPU released; waiting for warmup");

    // Phase 9 — wait for the warmup HCI event. We block on the IRQ
    // notification, drain the RX ring on each wake, and stop once we've
    // consumed at least three bytes whose first byte is 0x04 (HCI Event
    // packet indicator).
    wait_for_warmup()?;
    info!("phase 9 warmup HCI event received");

    // Phase 10 — post-init.
    controller::post_init();
    info!("phase 10 post-init done");

    Ok(())
}

/// Drive `sys_recv_open` to wait for MAILBOX2_CH1 IRQ notifications,
/// drain the LCPU→HCPU ring on each wake, and stop once we see the
/// warmup HCI event header.
fn wait_for_warmup() -> Result<(), LcpuInitError> {
    let mut wakes = 0u32;
    let mut header = [0u8; 3];
    let mut header_filled = 0usize;

    loop {
        if wakes >= WARMUP_MAX_WAKES {
            return Err(LcpuInitError::WarmupTimeout);
        }
        wakes += 1;

        // Block for the IRQ notification. We pass a closed receive
        // (`sys_recv_notification`) so no IPC messages can sneak in
        // ahead of the bringup completing.
        let bits = userlib::sys_recv_notification(MAILBOX2_BIT);
        if bits & MAILBOX2_BIT == 0 {
            continue;
        }

        // Acknowledge the hardware IRQ, drain the ring, re-arm the
        // notification.
        mailbox::ack_mailbox2_irq();
        userlib::sys_enable_irq(MAILBOX2_BIT);

        // Pull bytes until we have a 3-byte H4 header, then validate.
        while header_filled < 3 {
            let n = mailbox::read_hci(&mut header[header_filled..]);
            if n == 0 {
                break;
            }
            header_filled += n;
        }
        if header_filled < 3 {
            continue;
        }

        if header[0] != 0x04 {
            warn!("warmup frame had bad H4 type {}, expected 0x04", header[0]);
            return Err(LcpuInitError::WarmupBadFrame);
        }

        // Drain any parameter bytes the event carries so the ring is
        // empty before init() returns. The holder's first recv_hci can
        // observe later events without leftovers from warmup.
        let param_len = header[2] as usize;
        if param_len > 0 {
            let mut params = [0u8; 256];
            let cap = core::cmp::min(param_len, params.len());
            let mut got = 0usize;
            // Bounded retries so a missing tail doesn't deadlock here.
            for _ in 0..16 {
                let n = mailbox::read_hci(&mut params[got..cap]);
                got += n;
                if got >= cap {
                    break;
                }
                let bits = userlib::sys_recv_notification(MAILBOX2_BIT);
                if bits & MAILBOX2_BIT != 0 {
                    mailbox::ack_mailbox2_irq();
                    userlib::sys_enable_irq(MAILBOX2_BIT);
                }
            }
        }

        return Ok(());
    }
}

/// Best-effort cleanup after a failed bringup. Mirrors the `Drop` body
/// minus the holder/atomic clears (init() does those after).
fn bringup_teardown() {
    if let Err(e) = bringup::lcpu_reset_and_halt() {
        warn!("teardown reset_and_halt failed: {:?}", e);
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    info!("starting");
    ipc::server! {
        Lcpu: LcpuResource,
        @irq(lpsys_mailbox2_ch1) => || {
            // Drain MISR / clear ICR.
            mailbox::ack_mailbox2_irq();
            // Post to reactor
            Reactor::refresh(generated::notifications::GROUP_ID_LCPU_DATA, 0, 50, OverflowStrategy::Reject);
        },
    }
}
