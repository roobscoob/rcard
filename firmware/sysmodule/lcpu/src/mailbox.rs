//! HCI mailbox transport (qid 0).
//!
//! Two ring buffers in shared memory:
//! - **TX** (HCPU→LCPU): a `CircularBuf` header + 496 B payload owned by
//!   the lcpu task and statically allocated (`HCPU_TX`). Its address is
//!   written into ROM-config field +200 so LCPU knows where to read.
//! - **RX** (LCPU→HCPU): at `addr::LCPU2HCPU_MB_CH1` in LPSYS_SRAM.
//!   LCPU writes; we read.
//!
//! Doorbell:
//! - HCPU→LCPU: write `1 << qid` to `MAILBOX1.itr(0)` (channel 1, qid 0
//!   = bit 0).
//! - LCPU→HCPU: LCPU writes the same bit to MAILBOX2; the kernel raises
//!   IRQ 58 (MAILBOX2_CH1) which the lcpu task handles via the `@irq`
//!   arm of the IPC server.

use core::sync::atomic::{Ordering, fence};

use sifli_pac::{MAILBOX1, MAILBOX2};

use crate::addr;
use crate::circular_buf::{CircularBuf, CircularBufMutPtrExt};

/// HCI lives on qid 0 (mailbox bit 0 of channel 1 on both directions).
pub const HCI_QID: u8 = 0;

/// HCPU→LCPU TX ring (header + payload, 512 B total). The struct is
/// placed in normal HCPU SRAM (the lcpu task's data region); its
/// runtime address is resolved at link time and written into ROM-config
/// field +200 so LCPU knows where to read commands from.
#[repr(C, align(4))]
struct TxRing {
    header: CircularBuf,
    payload: [u8; addr::IPC_MB_BUF_SIZE - core::mem::size_of::<CircularBuf>()],
}

#[unsafe(link_section = ".data")]
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

/// Initialize the HCPU-side TX ring's `CircularBuf` header. After this,
/// LCPU (via its translated alias) can immediately read pending writes.
/// Idempotent — re-initializing zeroes the indices.
pub fn init_tx_ring() {
    let cb_ptr = (&raw mut HCPU_TX) as *mut CircularBuf;
    let payload_size =
        (addr::IPC_MB_BUF_SIZE - core::mem::size_of::<CircularBuf>()) as i16;

    // wr_buffer_ptr stores the HCPU view of the payload (we write here).
    let pool_wr = unsafe { (&raw mut HCPU_TX.payload) as *mut u8 };
    // rd_buffer_ptr stores the LCPU view (the address LCPU reads from).
    let pool_rd = (pool_wr as usize + addr::HCPU_TO_LCPU_OFFSET) as *mut u8;

    unsafe {
        cb_ptr.wr_init(pool_wr, payload_size);
        cb_ptr.rd_init(pool_rd);
    }
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
pub fn write_hci(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    let cb_ptr = (&raw mut HCPU_TX) as *mut CircularBuf;
    let n = unsafe { cb_ptr.put(data) };
    if n > 0 {
        // Doorbell: tell LCPU to drain qid 0.
        let qid_mask = 1u32 << HCI_QID;
        fence(Ordering::SeqCst);
        MAILBOX1
            .itr(0)
            .write_value(sifli_pac::mailbox::regs::Ixr(qid_mask));
    }
    n
}

/// Drain the LCPU→HCPU ring at `addr::LCPU2HCPU_MB_CH1` into `out`.
/// Returns the number of bytes copied. Caller is responsible for any
/// MAILBOX2 IRQ acknowledgement (done in the IRQ arm).
pub fn read_hci(out: &mut [u8]) -> usize {
    if out.is_empty() {
        return 0;
    }
    let cb_ptr = addr::LCPU2HCPU_MB_CH1 as *mut CircularBuf;
    unsafe { cb_ptr.get(out) }
}

/// Acknowledge the MAILBOX2_CH1 IRQ for any qids that triggered. Called
/// from the `@irq` arm before draining and again from the manual
/// warmup-wait loop in `init()`.
pub fn ack_mailbox2_irq() {
    let misr = MAILBOX2.misr(0).read().0;
    if misr != 0 {
        MAILBOX2
            .icr(0)
            .write_value(sifli_pac::mailbox::regs::Ixr(misr));
        fence(Ordering::SeqCst);
    }
}
