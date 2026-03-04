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
}

#[link_section = ".vector_table.interrupts"]
#[no_mangle]
pub static __INTERRUPTS: [Vector; 99] = [Vector {
    handler: DefaultHandler,
}; 99];

fn clock_setup() -> u32 {
    // SF32LB52 boots from internal RC oscillator at ~48 MHz.
    // TODO: configure PLL for 240 MHz when needed.
    48_000
}

#[entry]
fn main() -> ! {
    let cycles_per_ms = clock_setup();
    unsafe { hubris_kern::startup::start_kernel(cycles_per_ms) }
}
