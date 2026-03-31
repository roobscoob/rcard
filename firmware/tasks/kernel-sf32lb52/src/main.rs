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

    let cycles_per_ms = clock_setup();
    unsafe { hubris_kern::startup::start_kernel(cycles_per_ms) }
}
