#![no_std]
#![no_main]

mod logger;

use cortex_m_rt::entry;
use sf32lb52_pac as _;

#[entry]
fn main() -> ! {
    defmt::error!("hello from fob");

    loop {}
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
