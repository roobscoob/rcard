#![no_std]
#![no_main]

use cortex_m_rt::entry;

// The PAC's __INTERRUPTS is [Vector; 0] (SVD lacks interrupt definitions).
// Provide a proper vector table with 99 entries for the SF32LB52.
#[derive(Copy, Clone)]
#[repr(C)]
pub union Vector {
    handler: unsafe extern "C" fn(),
    reserved: u32,
}

unsafe impl Sync for Vector {}

extern "C" {
    fn DefaultHandler();
    static __reset_vector: u32;
}

#[repr(C, align(128))]
pub struct VectorTable([Vector; 99]);

#[link_section = ".vector_table.interrupts"]
#[no_mangle]
pub static __INTERRUPTS: VectorTable = VectorTable(
    [Vector {
        handler: DefaultHandler,
    }; 99],
);

fn clock_setup() -> u32 {
    // SF32LB52 boots from internal RC oscillator at ~48 MHz.
    // TODO: configure PLL for 240 MHz when needed.
    48_000
}

// PA09 I2C UART = Fn 4

const VECTOR_TABLE_OFFSET_REGISTER: *mut u32 = 0xE000_ED08 as *mut u32;

#[entry]
fn main() -> ! {
    // Point the CPU at our vector table (VTOR = SCB + 0x08)
    unsafe {
        // __reset_vector is at .vector_table + 0x08; subtract 8 to get the base
        core::ptr::write_volatile(
            VECTOR_TABLE_OFFSET_REGISTER,
            (&__reset_vector as *const u32 as u32) - 8,
        );
    }

    /// HPSYS_PINMUX base address
    const HPSYS_PINMUX: usize = 0x5000_3000;
    /// HPSYS_CFG base address
    const HPSYS_CFG: usize = 0x5000_B000;

    /// PAD_PA09 register (HPSYS_PINMUX + 0x58)
    const PAD_PA09: *mut u32 = (HPSYS_PINMUX + 0x58) as *mut u32;
    /// USART2_PINR register (HPSYS_CFG + 0x5C)
    const USART2_PINR: *mut u32 = (HPSYS_CFG + 0x5C) as *mut u32;

    /// FSEL value for PA_I2C_UART function
    const FSEL_I2C_UART: u32 = 4;

    /// Configure PA09 as USART2 TX.
    ///
    /// Two-step mux on the SF32LB52:
    ///   1. PAD_PA09.FSEL[3:0] = 4 (PA_I2C_UART)
    ///   2. USART2_PINR.TXD_PIN[5:0] = 9 (PA09)
    pub unsafe fn pa09_as_usart2_tx() {
        // Step 1: Set PAD_PA09 FSEL to PA_I2C_UART (4)
        let mut val = PAD_PA09.read_volatile();
        val &= !0xF; // clear FSEL[3:0]
        val |= FSEL_I2C_UART;
        PAD_PA09.write_volatile(val);

        // Step 2: Route USART2 TXD to PA09 via USART2_PINR.TXD_PIN[5:0]
        let mut val = USART2_PINR.read_volatile();
        val &= !0x3F; // clear TXD_PIN[5:0]
        val |= 9; // PA09
        USART2_PINR.write_volatile(val);
    }

    unsafe { pa09_as_usart2_tx() };

    let cycles_per_ms = clock_setup();
    unsafe { hubris_kern::startup::start_kernel(cycles_per_ms) }
}
