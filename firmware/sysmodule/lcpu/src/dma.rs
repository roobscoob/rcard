//! Minimal blocking DMA shim for `rf_cal::txdc`.
//!
//! Substitute for `sifli_hal::dma::{Channel, Transfer, Increment,
//! TransferOptions}` as used in
//! `sifli-rs/sifli-radio/src/bluetooth/rf_cal/txdc.rs:23-25, 161-191` @
//! commit aa4c19c. License: Apache-2.0 (upstream).
//! See `LICENSES/SIFLI-RS-APACHE-2.0.txt`.
//!
//! Differences from upstream:
//! - **Blocking only.** sifli-rs's `Transfer::blocking_wait()` is called
//!   inside `capture_adc_samples`; we just expose one synchronous
//!   function and drop the embassy-async machinery (`PeripheralRef`,
//!   `Channel` trait, `TransferOptions` struct, interrupt routing).
//! - **DMAC2-only**, single API call. sifli-rs's HAL supports DMAC1
//!   and DMAC2 with arbitrary channels; we hardcode DMAC2 because
//!   that's the only DMA controller lcpu's MPU sees (it's inside the
//!   `lpsys` aggregate at 0x40001000, see chip.ncl). DMAC1 lives in
//!   HPSYS and we don't grant access to it.
//! - **No CSELR programming.** TXDC's source is the PHY RX dump buffer
//!   at 0x400C0000, which DMA reads as a fixed-address peripheral
//!   without a hardware request line. We use MEM2MEM=1 (software-
//!   triggered, fire-and-forget) with PINC=0, MINC=1 so the channel
//!   races through the 512-word read at peak bus speed. This matches
//!   the SDK's `bt_rfc_txdc_cal` behavior (per sifli-rs comments and
//!   the SDK source it cites).
//! - **No DMA error handling beyond returning success.** If TEIF
//!   asserts we still complete the call; TXDC's result is just
//!   nonsense in that case and the search loop will pick the wrong
//!   offset. Bringup will fail loudly downstream. Acceptable
//!   tradeoff for the simplicity of the shim.

use sifli_pac::DMAC2;
use sifli_pac::dmac::regs::{Cm0ar, Cndtr, Cpar};
use sifli_pac::dmac::vals::{Dir, Pl, Size};

/// Polling budget for transfer completion. At 24 MHz LPSYS HCLK,
/// 512 × 4 B = 2 KiB at ~peak bus speed should complete in well under
/// 100 µs; 1_000_000 polls (~4 ms at 240 MHz HCPU clock) is wildly
/// generous but bounded.
const POLL_BUDGET: u32 = 1_000_000;

/// One of DMAC2's eight channels (0..=7).
///
/// `txdc_cal_full` only needs one channel — we hand out channel 7 from
/// `claim_default()`. If/when we add more DMA users to lcpu we can
/// upgrade this to a small bitmap allocator.
#[derive(Copy, Clone, Debug)]
pub struct DmacChannel(u8);

impl DmacChannel {
    /// Claim a channel by raw index. Caller is responsible for not
    /// double-using channels concurrently. RF cal runs single-threaded
    /// during bringup, so contention isn't possible there.
    pub const fn new(index: u8) -> Self {
        // Hubris workspace lints deny `panic`; replace with a const
        // assertion via array indexing so out-of-range hits at compile
        // time where possible. At runtime, indices 0..8 are valid.
        let _bound: [(); 8] = [(); 8];
        // (Note: can't actually compile-check `index < 8` for a runtime
        // u8 in a const fn without panic; we trust the caller.)
        let _ = _bound;
        Self(index)
    }

    /// Default channel for the RF cal flow.
    pub const fn claim_default() -> Self {
        Self(7)
    }

    /// Blocking peripheral→memory transfer with memory increment.
    ///
    /// - `src`: source address (e.g. PHY RX dump @ 0x400C0000). Read
    ///   as a fixed `u32`-aligned address.
    /// - `dst`: destination address (e.g. EM buffer). Incremented per
    ///   transferred word.
    /// - `count`: number of `u32` words to transfer.
    ///
    /// Returns when DMAC2.ISR.TCIF(channel) asserts, or after
    /// `POLL_BUDGET` polls — whichever happens first.
    ///
    /// # Safety
    /// Caller must ensure both `src` and `dst` are valid for `count`
    /// consecutive `u32` accesses, that DMA reads from `src` and
    /// writes to `dst` are sound (no aliasing/no concurrent CPU
    /// access during the transfer), and that the channel isn't
    /// already armed for a different transfer.
    pub unsafe fn transfer_u32(
        &mut self,
        src: *const u32,
        dst: *mut u32,
        count: usize,
    ) {
        let ch = self.0 as usize;
        debug_assert!(ch < 8, "DMAC2 channel out of range");
        debug_assert!(count <= u16::MAX as usize, "CNDTR is 16-bit");

        // Disable the channel before reprogramming (per DMA reference:
        // CCR.EN must be 0 to safely update CNDTR/CPAR/CM0AR/control).
        DMAC2.ccr(ch).modify(|w| w.set_en(false));
        while DMAC2.ccr(ch).read().en() {}

        // Clear any latched flags from a prior run. Writing CGIF
        // clears all of (TEIF, HTIF, TCIF, GIF) for the channel.
        DMAC2.ifcr().write(|w| w.set_cgif(ch, true));

        // Program addresses + count.
        DMAC2.cpar(ch).write_value(Cpar(src as u32));
        DMAC2.cm0ar(ch).write_value(Cm0ar(dst as u32));
        DMAC2.cndtr(ch).write_value(Cndtr(count as u32));

        // Control: 32-bit transfers, memory increments, peripheral
        // fixed, MEM2MEM software-trigger (no request line for the PHY
        // RX dump buffer). Priority Low matches sifli-rs's
        // `dma_opts()`. EN=1 starts the transfer immediately under
        // MEM2MEM.
        DMAC2.ccr(ch).write(|w| {
            w.set_dir(Dir::PeripheralToMemory);
            w.set_pinc(false);
            w.set_minc(true);
            w.set_psize(Size::Bits32);
            w.set_msize(Size::Bits32);
            w.set_pl(Pl::Low);
            w.set_circ(false);
            w.set_mem2mem(true);
            w.set_tcie(false);
            w.set_htie(false);
            w.set_teie(false);
            w.set_en(true);
        });

        // Poll TCIF with a bounded budget.
        let mut polls = 0u32;
        while !DMAC2.isr().read().tcif(ch) {
            polls += 1;
            if polls >= POLL_BUDGET {
                break;
            }
        }

        // Clear TCIF and disable the channel for the next caller.
        DMAC2.ifcr().write(|w| w.set_cgif(ch, true));
        DMAC2.ccr(ch).modify(|w| w.set_en(false));
    }
}
