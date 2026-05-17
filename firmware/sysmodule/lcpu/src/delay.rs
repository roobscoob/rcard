//! Tiny blocking-delay helper. Substitute for
//! `sifli_hal::cortex_m_blocking_delay_us`
//! (`sifli-rs/sifli-hal/src/lib.rs:176-180` @ aa4c19c).
//!
//! Differences from upstream:
//! - sifli-hal queries `rcc::get_hclk_freq()` at runtime; we hardcode
//!   `HCPU_HCLK_HZ = 240_000_000` because (a) the Renode model in
//!   `firmware/chips/sf32lb52.ncl` pins the USART frequency to that, and
//!   (b) `bringup.rs`'s POLL_BUDGET comment already assumes "the inner
//!   waits clock at 240 MHz". If we ever DVFS the HPSYS clock at
//!   runtime, this needs to consult a live source.
//! - Used only by the vendored `rf_cal/*` code — direct `cortex_m::asm::delay`
//!   calls elsewhere in lcpu pre-date this helper and stay as-is.

const HCPU_HCLK_HZ: u32 = 240_000_000;

/// Blocking spin for approximately `us` microseconds at HPSYS HCLK.
#[inline]
pub fn delay_us(us: u32) {
    // 240 cycles ≈ 1 µs at 240 MHz. The asm::delay loop is 1 cycle per
    // iteration on Cortex-M, modulo branch-prediction noise.
    let cycles = (HCPU_HCLK_HZ as u64).saturating_mul(us as u64) / 1_000_000;
    cortex_m::asm::delay(cycles as u32);
}
