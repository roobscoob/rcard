//! HCI mailbox transport (qid 0).
//!
//! Two ring buffers in shared memory:
//! - **TX** (HCPU→LCPU): a `CircularBuf` header + 496 B payload owned by
//!   the lcpu task and statically allocated (`HCPU_TX`). Its address is
//!   written into ROM-config field +200 (Letter) or post-boot
//!   `G_ROM_CONFIG_A3` (A3) so LCPU knows where to read.
//! - **RX** (LCPU→HCPU): in LPSYS_SRAM at a chip-rev-specific address.
//!   Letter uses `0x2040_2800`; A3 uses `0x2040_5C00`. Resolved at
//!   `init_tx_ring(rev)` and stashed in `RX_ADDR`.
//!
//! Doorbell:
//! - HCPU→LCPU: write `1 << qid` to `MAILBOX1.itr(0)` (channel 1, qid 0
//!   = bit 0).
//! - LCPU→HCPU: LCPU writes the same bit to MAILBOX2; the kernel raises
//!   IRQ 58 (MAILBOX2_CH1) which the lcpu task handles via the `@irq`
//!   arm of the IPC server.

use core::sync::atomic::{AtomicUsize, Ordering, fence};

use rcard_log::{info, warn};
use sifli_pac::{HPSYS_AON, LPSYS_AON, MAILBOX1, MAILBOX2};

use sysmodule_syscon_api::ChipRev;

use crate::addr;
use crate::bringup;
use crate::circular_buf::{CircularBuf, CircularBufMutPtrExt, CircularBufPtrExt};

/// HCI lives on qid 0 (mailbox bit 0 of channel 1 on both directions).
pub const HCI_QID: u8 = 0;

/// Chosen LCPU→HCPU ring address. Set by `init_tx_ring(rev)`; read by
/// `read_hci` and the diagnostic dump in `main.rs`.
static RX_ADDR: AtomicUsize = AtomicUsize::new(0);

/// HCPU→LCPU TX ring (header + payload, 512 B total). The struct is
/// placed in normal HCPU SRAM (the lcpu task's data region); its
/// runtime address is resolved at link time and written into ROM-config
/// field +200 so LCPU knows where to read commands from.
#[repr(C, align(4))]
struct TxRing {
    header: CircularBuf,
    payload: [u8; addr::IPC_MB_BUF_SIZE - core::mem::size_of::<CircularBuf>()],
}

#[unsafe(link_section = ".hcpu_tx")]
static mut HCPU_TX: TxRing = TxRing {
    header: CircularBuf {
        rd_buffer_ptr: core::ptr::null_mut(),
        wr_buffer_ptr: core::ptr::null_mut(),
        read_idx_mirror: 0,
        write_idx_mirror: 0,
        buffer_size: 0,
    },
    payload: [0u8; addr::IPC_MB_BUF_SIZE - core::mem::size_of::<CircularBuf>()],
};

/// HCPU view of the TX ring's header — the address LCPU sees is this
/// plus `HCPU_TO_LCPU_OFFSET`. Returned by [`tx_ring_hcpu_addr`] and
/// written into ROM-config +200.
pub fn tx_ring_hcpu_addr() -> u32 {
    (&raw const HCPU_TX) as u32
}

/// HCPU view of the RX (LCPU→HCPU) ring. Valid after `init_tx_ring`.
/// Used by the diagnostic dump in `main.rs`. Returns 0 if uninitialized.
pub fn rx_ring_hcpu_addr() -> usize {
    RX_ADDR.load(Ordering::Acquire)
}

/// Initialize the HCPU-side TX ring's `CircularBuf` header and stash
/// the chip-rev-correct RX ring address. After this, LCPU (via its
/// translated alias) can immediately read pending writes. Idempotent —
/// re-initializing zeroes the indices.
pub fn init_tx_ring(rev: ChipRev) {
    let cb_ptr = (&raw mut HCPU_TX) as *mut CircularBuf;
    let payload_size = (addr::IPC_MB_BUF_SIZE - core::mem::size_of::<CircularBuf>()) as i16;

    // wr_buffer_ptr stores the HCPU view of the payload (we write here).
    let pool_wr = unsafe { (&raw mut HCPU_TX.payload) as *mut u8 };
    // rd_buffer_ptr stores the LCPU view (the address LCPU reads from).
    let pool_rd = (pool_wr as usize + addr::HCPU_TO_LCPU_OFFSET) as *mut u8;

    unsafe {
        cb_ptr.wr_init(pool_wr, payload_size);
        cb_ptr.rd_init(pool_rd);
    }

    RX_ADDR.store(addr::lcpu2hcpu_mb_ch1(rev), Ordering::Release);
}

/// Unmask qid 0 on both mailboxes and clear any stale RX pending bits
/// before LCPU is released.
pub fn unmask_hci_qid() {
    let qid_mask = 1u32 << HCI_QID;
    MAILBOX1.ier(0).modify(|w| w.0 |= qid_mask);
    MAILBOX2.ier(0).modify(|w| w.0 |= qid_mask);
    MAILBOX2
        .icr(0)
        .write_value(sifli_pac::mailbox::regs::Ixr(qid_mask));
}

/// Push `data` onto the HCPU→LCPU ring. Returns the number of bytes
/// written; if 0, the ring was full. Always doorbells when at least one
/// byte landed — partial frames must be flushed so LCPU can drain them.
///
/// After the doorbell, spin-polls the ring's `read_idx_mirror` (written
/// by LCPU as it drains) and `HPSYS_AON.ISSR.lp_active`, logging both
/// before and after states. This is diagnostic — the wakeup mechanism
/// is supposed to be self-contained (MAILBOX1 IRQ in LCPU's NVIC wakes
/// it from WFI), so we want loud visibility if LCPU never consumes.
pub fn write_hci(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    let cb_ptr = (&raw mut HCPU_TX) as *mut CircularBuf;
    let cb_const = cb_ptr.cast_const();

    // Snapshot pre-doorbell state.
    let read_idx_before = unsafe { cb_const.read_idx_mirror() };
    let lp_active_before = HPSYS_AON.issr().read().lp_active();

    let n = unsafe { cb_ptr.put(data) };
    if n == 0 {
        return 0;
    }

    let write_idx_after_put = unsafe { cb_const.write_idx_mirror() };

    // Doorbell: tell LCPU to drain qid 0.
    let qid_mask = 1u32 << HCI_QID;
    fence(Ordering::SeqCst);
    MAILBOX1
        .itr(0)
        .write_value(sifli_pac::mailbox::regs::Ixr(qid_mask));

    info!(
        "doorbell: wrote {} bytes; pre-state read_idx={} write_idx={} lp_active={} mbox_misr={}",
        n,
        read_idx_before,
        write_idx_after_put,
        lp_active_before as u8,
        MAILBOX1.misr(0).read().0,
    );

    // Periodic snapshot of LCPU state until either it drains the ring
    // or we exhaust the budget. Each snapshot logs: lp_active (HPSYS,
    // always readable), MAILBOX1.misr (HPSYS, always readable — bit 0
    // stays high until LCPU acks the doorbell IRQ), and conditional on
    // lp_active being high, LPSYS_AON fields (pwr_mode, sleep_status,
    // pcr value, cpuwait). PCR is the AON-held PC pointer the chip
    // uses to relaunch LCPU after sleep — if it changes between
    // snapshots LCPU is being put through sleep/wake cycles; if it
    // stays put LCPU is running continuously (or stuck).
    const SNAPSHOT_INTERVAL: u32 = 1_000;
    const SNAPSHOT_DELAY_CYCLES: u32 = 240; // ~1 µs at 240 MHz → ~1 ms per interval
    const MAX_SNAPSHOTS: u32 = 30; // ~30 ms total budget

    let mut polls = 0u32;
    let mut snapshots = 0u32;
    let drained;
    loop {
        let read_idx_now = unsafe { cb_const.read_idx_mirror() };
        if read_idx_now != read_idx_before {
            info!(
                "LCPU drained ring after {} polls: read_idx {} -> {}",
                polls, read_idx_before, read_idx_now,
            );
            drained = true;
            break;
        }

        if polls > 0 && polls % SNAPSHOT_INTERVAL == 0 {
            log_lcpu_snapshot(snapshots, polls);
            snapshots += 1;
            if snapshots >= MAX_SNAPSHOTS {
                drained = false;
                break;
            }
        }

        polls += 1;
        cortex_m::asm::delay(SNAPSHOT_DELAY_CYCLES);
    }

    if !drained {
        warn!(
            "LCPU never drained the ring after {} polls ({} snapshots)",
            polls, snapshots,
        );
    }

    n
}

/// One-line snapshot of everything the HCPU can observe about LCPU.
/// LPSYS_AON access is gated on `lp_active`: when the LP domain is
/// powered down, reads to `0x4004_xxxx` fault. HPSYS_AON and MAILBOX1
/// are in HPSYS and always readable.
fn log_lcpu_snapshot(snapshot_idx: u32, polls: u32) {
    let issr = HPSYS_AON.issr().read();
    let lp_active = issr.lp_active();
    let mbox_misr = MAILBOX1.misr(0).read().0;

    if lp_active {
        let pmr = LPSYS_AON.pmr().read();
        let slp = LPSYS_AON.slp_ctrl().read();
        let pc = LPSYS_AON.pcr().read().pc();
        info!(
            "[snap {}, poll {}] lp_active=1 mbox_misr={} pmr_mode={} cpuwait={} sleep_status={} bt_wkup={} aon_pc={}",
            snapshot_idx,
            polls,
            mbox_misr,
            pmr.mode(),
            pmr.cpuwait() as u8,
            slp.sleep_status() as u8,
            slp.bt_wkup() as u8,
            pc,
        );
    } else {
        info!(
            "[snap {}, poll {}] lp_active=0 mbox_misr={} (LP domain off — LPSYS regs skipped)",
            snapshot_idx, polls, mbox_misr,
        );
    }
}

/// Drain the LCPU→HCPU ring (chip-rev-specific address, set by
/// `init_tx_ring`) into `out`. Returns the number of bytes copied.
/// Returns 0 if `init_tx_ring` hasn't been called.
///
/// Wraps the LPSYS-RAM read in a per-call wake hold so HCPU can reach
/// LCPU's shared ring even if LCPU has dropped into LP sleep since it
/// last doorbelled.
pub fn read_hci(out: &mut [u8]) -> usize {
    if out.is_empty() {
        return 0;
    }
    let rx = RX_ADDR.load(Ordering::Acquire);
    if rx == 0 {
        return 0;
    }
    let cb_ptr = rx as *mut CircularBuf;
    bringup::hold_lcpu_awake();
    let n = unsafe { cb_ptr.get(out) };
    bringup::release_lcpu_hold();
    n
}

/// Acknowledge the MAILBOX2_CH1 IRQ for any qids that triggered. Called
/// from the `@irq` arm before draining and again from the manual
/// warmup-wait loop in `init()`.
///
/// MAILBOX2 sits in the LPSYS peripheral domain and is unreachable from
/// HCPU while LCPU is in LP sleep, so we briefly assert the wake hold
/// around the MISR/ICR access.
pub fn ack_mailbox2_irq() {
    bringup::hold_lcpu_awake();
    let misr = MAILBOX2.misr(0).read().0;
    if misr != 0 {
        MAILBOX2
            .icr(0)
            .write_value(sifli_pac::mailbox::regs::Ixr(misr));
        fence(Ordering::SeqCst);
    }
    bringup::release_lcpu_hold();
}
