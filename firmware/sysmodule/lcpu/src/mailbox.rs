//! HCI mailbox transport (qid 0).
//!
//! Two ring buffers in shared memory:
//! - **TX** (HCPU→LCPU): a `CircularBuf` header + 492 B payload at the
//!   fixed peripheral address `0x2007_FE00` (the `HCPU_TX` static, see
//!   below). Declared as a peripheral region in `chips/sf32lb52.ncl`
//!   and owned by the lcpu task via `task.ncl`. Peripheral mapping is
//!   non-cacheable so LCPU's updates to `read_idx_mirror` (via its
//!   `+HCPU_TO_LCPU_OFFSET` alias) are immediately visible to HCPU —
//!   previously the region was placed via `#[link_section]` in
//!   cacheable SRAM and LCPU's writes to the header got masked by
//!   HCPU's stale D-cache.
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
use sifli_pac::{HPSYS_AON, MAILBOX1, MAILBOX2};
// LPSYS_AON only referenced by the commented-out snapshot helper below.

use sysmodule_syscon_api::ChipRev;

use crate::addr;
use crate::bringup;
use crate::circular_buf::{CircularBuf, CircularBufMutPtrExt, CircularBufPtrExt};

/// HCI lives on qid 0 (mailbox bit 0 of channel 1 on both directions).
pub const HCI_QID: u8 = 0;

/// Chosen LCPU→HCPU ring address. Set by `init_tx_ring(rev)`; read by
/// `read_hci` and the diagnostic dump in `main.rs`.
static RX_ADDR: AtomicUsize = AtomicUsize::new(0);

/// Fixed-address handle to the HCPU→LCPU ring. `chips/sf32lb52.ncl`
/// reserves `0x2007_FE00..+512` as the `hcpu_tx` peripheral region and
/// `task.ncl` grants it exclusively to the lcpu task. Peripheral mapping
/// makes the region non-cacheable from HCPU's side so LCPU's updates
/// (via its `+HCPU_TO_LCPU_OFFSET` alias) stay coherent — previously,
/// when this lived in cacheable SRAM via `#[link_section]`,
/// `read_idx_mirror` stayed stale at HCPU because the cache line never
/// got invalidated.
#[repr(C, align(4))]
struct TxRing {
    header: CircularBuf,
    payload: [u8; addr::IPC_MB_BUF_SIZE - core::mem::size_of::<CircularBuf>()],
}

/// Newtype around a raw pointer so we can put it in a `static` (raw
/// pointers themselves are `!Sync`). A `&'static TxRing` would be the
/// natural fit, but const-eval's strict-provenance check rejects
/// constructing a reference from a literal integer address — raw
/// pointers carry no such restriction.
#[repr(transparent)]
struct TxRingPtr(*mut TxRing);
unsafe impl Sync for TxRingPtr {}

impl TxRingPtr {
    /// Raw mutable pointer to the ring.
    fn ring(&self) -> *mut TxRing {
        self.0
    }

    /// Raw mutable pointer to the `CircularBuf` header — it's the first
    /// field of `TxRing` (`#[repr(C)]`) so they share a base address.
    fn header(&self) -> *mut CircularBuf {
        self.0.cast()
    }
}

/// SAFETY: `0x2007_FE00` is the base of the `hcpu_tx` peripheral
/// region declared in `chips/sf32lb52.ncl` and exclusively owned by
/// this task in `task.ncl`, so the pointer is valid and unaliased for
/// the lifetime of the program. Dereferences are gated by the single
/// `unsafe` block in `init_tx_ring` plus the raw-pointer-only accesses
/// in `write_hci`.
static HCPU_TX: TxRingPtr = TxRingPtr(0x2007_FE00 as *mut TxRing);

/// HCPU view of the TX ring's header. Written into ROM-config +200
/// (Letter) / `G_ROM_CONFIG_A3` (A3) so LCPU knows where to read
/// commands from.
pub fn tx_ring_hcpu_addr() -> u32 {
    HCPU_TX.ring() as u32
}

/// HCPU view of the RX (LCPU→HCPU) ring. Valid after `init_tx_ring`.
/// Used by the diagnostic dump in `main.rs`. Returns 0 if uninitialized.
pub fn rx_ring_hcpu_addr() -> usize {
    RX_ADDR.load(Ordering::Acquire)
}

/// Zero the TX ring memory (peripheral SRAM is undefined at reset) and
/// initialize the `CircularBuf` header so LCPU can immediately read
/// pending writes via its `+HCPU_TO_LCPU_OFFSET` alias. Also stashes
/// the rev-correct RX ring address. Idempotent — re-initializing
/// zeroes the indices.
pub fn init_tx_ring(rev: ChipRev) {
    let payload_size = (addr::IPC_MB_BUF_SIZE - core::mem::size_of::<CircularBuf>()) as i16;

    // SAFETY: HCPU_TX is exclusively owned by this task and not aliased
    // anywhere else, so we have a single live mutator. The peripheral
    // memory's contents are undefined at reset; one `write_bytes` clears
    // both halves of the header (rd/wr indices, pointers, size) before
    // `wr_init`/`rd_init` re-populate the fields we care about.
    unsafe {
        let ring = HCPU_TX.ring();
        core::ptr::write_bytes(ring as *mut u8, 0, core::mem::size_of::<TxRing>());

        let cb_ptr = HCPU_TX.header();
        let pool_wr = core::ptr::addr_of_mut!((*ring).payload) as *mut u8;
        let pool_rd = (pool_wr as usize + addr::HCPU_TO_LCPU_OFFSET) as *mut u8;
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
/// written; if 0, the ring was full.
///
/// **Diagnostic mode**: after the doorbell, tight-spin in two phases:
/// 1. count cycles until `HPSYS_AON.ISSR.lp_active` goes high (LCPU woke);
/// 2. count cycles until it goes low again (LCPU returned to sleep).
///
/// This is to test the theory that LCPU briefly wakes, clears the
/// MAILBOX1 IRQ pending bit, fails to drain the ring, and returns to
/// sleep — all before any of our previous periodic snapshots managed to
/// catch lp_active high.
pub fn write_hci(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    info!("write_hci: entered, data.len={}", data.len());
    let cb_ptr = HCPU_TX.header();
    let cb_const = cb_ptr.cast_const();

    // TX hdr pre-put — lets us see whether wr_idx/rd_idx are sane before
    // we touch them, in case prior state left the ring corrupted.
    unsafe {
        let rd_im = cb_const.read_idx_mirror();
        let wr_im = cb_const.write_idx_mirror();
        let size = cb_const.buffer_size();
        info!(
            "write_hci: TX hdr pre-put: rd_idx_mirror={} wr_idx_mirror={} size={}",
            rd_im, wr_im, size,
        );
    }

    let n = unsafe { cb_ptr.put(data) };
    info!("write_hci: post-put n={}", n);
    if n == 0 {
        return 0;
    }

    let lp_active_before = HPSYS_AON.issr().read().lp_active();
    info!("write_hci: lp_active_before={}", lp_active_before as u8);

    // Doorbell: tell LCPU to drain qid 0.
    let qid_mask = 1u32 << HCI_QID;
    fence(Ordering::SeqCst);
    MAILBOX1
        .itr(0)
        .write_value(sifli_pac::mailbox::regs::Ixr(qid_mask));
    info!("write_hci: doorbell written");

    // Phase 1: spin until lp_active goes high. Budget ~200 ms at 240 MHz
    // — the LP domain ramp from cold should be well under 1 ms.
    // const WAKE_BUDGET: u32 = 50_000_000;
    // let mut wake_cycles: u32 = 0;
    // let woke = loop {
    //     if HPSYS_AON.issr().read().lp_active() {
    //         break true;
    //     }
    //     if wake_cycles >= WAKE_BUDGET {
    //         break false;
    //     }
    //     wake_cycles += 1;
    // };
    // info!(
    //     "write_hci: phase 1 done, woke={} wake_cycles={}",
    //     woke as u8, wake_cycles
    // );

    // if !woke {
    //     warn!(
    //         "wake-phase: lp_active never went high after {} cycles (was {} before doorbell)",
    //         wake_cycles, lp_active_before as u8,
    //     );
    //     return n;
    // }

    // Phase 2: spin until lp_active goes low again. Shorter budget +
    // progress log every PROGRESS_INTERVAL iterations so we can tell
    // "stuck in tight spin" from "stuck downstream of phase 2".
    // const SLEEP_BUDGET: u32 = 5_000_000;
    // const PROGRESS_INTERVAL: u32 = 1_000_000;
    // let mut sleep_cycles: u32 = 0;
    // let mut last_progress: u32 = 0;
    // let slept = loop {
    //     if !HPSYS_AON.issr().read().lp_active() {
    //         break true;
    //     }
    //     if sleep_cycles >= SLEEP_BUDGET {
    //         break false;
    //     }
    //     sleep_cycles += 1;
    //     if sleep_cycles - last_progress >= PROGRESS_INTERVAL {
    //         info!(
    //             "write_hci: phase 2 still spinning, sleep_cycles={}",
    //             sleep_cycles
    //         );
    //         last_progress = sleep_cycles;
    //     }
    // };
    // info!(
    //     "write_hci: phase 2 done, slept={} sleep_cycles={}",
    //     slept as u8, sleep_cycles
    // );

    // if slept {
    //     info!(
    //         "wake/sleep: wake_cycles={} sleep_cycles={} (lp_active 0->1->0)",
    //         wake_cycles, sleep_cycles,
    //     );
    // } else {
    //     info!(
    //         "wake/sleep: wake_cycles={} then lp_active stayed high for {} cycles (budget exhausted)",
    //         wake_cycles, sleep_cycles,
    //     );
    // }

    // Dump the TX ring header so we can see if LCPU advanced
    // read_idx_mirror to match the post-put write_idx_mirror.
    // unsafe {
    //     let rd_im = cb_const.read_idx_mirror();
    //     let wr_im = cb_const.write_idx_mirror();
    //     info!(
    //         "TX hdr post-doorbell: rd_idx_mirror={} wr_idx_mirror={}",
    //         rd_im, wr_im,
    //     );
    // }

    n
}

// Original diagnostic code retained below for re-enabling once we've
// characterized the wake/sleep timing.
//
// const SNAPSHOT_INTERVAL: u32 = 1_000;
// const SNAPSHOT_DELAY_CYCLES: u32 = 240; // ~1 µs at 240 MHz → ~1 ms per interval
// const MAX_SNAPSHOTS: u32 = 30; // ~30 ms total budget
// let mut polls = 0u32;
// let mut snapshots = 0u32;
// loop {
//     let read_idx_now = unsafe { cb_const.read_idx_mirror() };
//     if read_idx_now != read_idx_before {
//         info!("LCPU drained ring after {} polls: read_idx {} -> {}",
//               polls, read_idx_before, read_idx_now);
//         break;
//     }
//     if polls > 0 && polls % SNAPSHOT_INTERVAL == 0 {
//         log_lcpu_snapshot(snapshots, polls);
//         snapshots += 1;
//         if snapshots >= MAX_SNAPSHOTS { break; }
//     }
//     polls += 1;
//     cortex_m::asm::delay(SNAPSHOT_DELAY_CYCLES);
// }

// Commented out alongside the diagnostic snapshot loop in `write_hci`.
// Re-enable together when going back to the periodic-snapshot strategy.
//
// /// One-line snapshot of everything the HCPU can observe about LCPU.
// /// LPSYS_AON access is gated on `lp_active`: when the LP domain is
// /// powered down, reads to `0x4004_xxxx` fault. HPSYS_AON and MAILBOX1
// /// are in HPSYS and always readable.
// fn log_lcpu_snapshot(snapshot_idx: u32, polls: u32) {
//     let issr = HPSYS_AON.issr().read();
//     let lp_active = issr.lp_active();
//     let mbox_misr = MAILBOX1.misr(0).read().0;
//     if lp_active {
//         let pmr = LPSYS_AON.pmr().read();
//         let slp = LPSYS_AON.slp_ctrl().read();
//         let pc = LPSYS_AON.pcr().read().pc();
//         info!("[snap {}, poll {}] lp_active=1 mbox_misr={} pmr_mode={} cpuwait={} sleep_status={} bt_wkup={} aon_pc={}",
//               snapshot_idx, polls, mbox_misr, pmr.mode(), pmr.cpuwait() as u8,
//               slp.sleep_status() as u8, slp.bt_wkup() as u8, pc);
//     } else {
//         info!("[snap {}, poll {}] lp_active=0 mbox_misr={} (LP domain off — LPSYS regs skipped)",
//               snapshot_idx, polls, mbox_misr);
//     }
// }

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
    let cb_const = cb_ptr.cast_const();
    let _wake = bringup::WakeLock::new();

    // Diagnostic: dump the LCPU-initialized header so we can see whether
    // rd_buffer_ptr is in HCPU view (0x2040_xxxx) or LCPU view
    // (0x0040_xxxx), and whether the indices actually differ. Reads are
    // volatile so we don't pick up cached/stale values.
    unsafe {
        let rd_ptr = cb_const.rd_buffer_ptr() as u32;
        let wr_ptr = cb_const.wr_buffer_ptr() as u32;
        let rd_im = cb_const.read_idx_mirror();
        let wr_im = cb_const.write_idx_mirror();
        let size = cb_const.buffer_size();
        info!(
            "RX hdr @ {}: rd_ptr={} wr_ptr={} rd_idx_mirror={} wr_idx_mirror={} size={}",
            rx as u32, rd_ptr, wr_ptr, rd_im, wr_im, size,
        );
    }

    unsafe { cb_ptr.get(out) }
}

/// Acknowledge the MAILBOX2_CH1 IRQ for any qids that triggered. Called
/// from the `@irq` arm before draining and again from the manual
/// warmup-wait loop in `init()`.
///
/// MAILBOX2 sits in the LPSYS peripheral domain and is unreachable from
/// HCPU while LCPU is in LP sleep, so we briefly assert the wake hold
/// around the MISR/ICR access.
pub fn ack_mailbox2_irq() {
    let _wake = bringup::WakeLock::new();
    let misr = MAILBOX2.misr(0).read().0;
    if misr != 0 {
        MAILBOX2
            .icr(0)
            .write_value(sifli_pac::mailbox::regs::Ixr(misr));
        fence(Ordering::SeqCst);
    }
}
