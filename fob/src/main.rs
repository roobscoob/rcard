#![no_std]
#![no_main]

use sf32lb52_pac::Usart1;

#[export_name = "main"]
fn main() -> ! {
    let usart = unsafe { Usart1::steal() };
    // BRR = 48MHz / 115200 = 417 (0x1A1)
    usart.brr().write(|w| unsafe { w.bits(0x1A1) });
    usart.cr1().write(|w| w.ue().set_bit().te().set_bit());

    for &b in b"hello from fob\r\n" {
        while usart.isr().read().txe().bit_is_clear() {}
        usart.tdr().write(|w| unsafe { w.bits(b as u32) });
    }

    loop {
        userlib::sys_recv_notification(0);
    }
}
