use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};
use sf32lb52_pac::Usart1;

defmt::timestamp!("za time");

static TAKEN: AtomicBool = AtomicBool::new(false);

struct SyncEncoder(UnsafeCell<defmt::Encoder>);
unsafe impl Sync for SyncEncoder {}
static ENCODER: SyncEncoder = SyncEncoder(UnsafeCell::new(defmt::Encoder::new()));

#[defmt::global_logger]
struct Logger;

fn send_bytes(bytes: &[u8]) {
    let usart = unsafe { Usart1::steal() };
    for &b in bytes {
        while usart.isr().read().txe().bit_is_clear() {}
        usart.tdr().write(|w| unsafe { w.bits(b as u32) });
    }
}

unsafe impl defmt::Logger for Logger {
    fn acquire() {
        cortex_m::interrupt::disable();

        if TAKEN.load(Ordering::Acquire) {
            panic!("Logger is already taken");
        }

        TAKEN.store(true, Ordering::Release);

        let usart = unsafe { Usart1::steal() };
        usart.cr1().write(|w| w.ue().set_bit().te().set_bit());

        unsafe { (*ENCODER.0.get()).start_frame(send_bytes) };
    }

    unsafe fn flush() {
        let usart = unsafe { Usart1::steal() };
        while usart.isr().read().txe().bit_is_clear() {}
    }

    unsafe fn release() {
        unsafe { (*ENCODER.0.get()).end_frame(send_bytes) };

        let usart = unsafe { Usart1::steal() };
        usart.cr1().write(|w| w.ue().clear_bit().te().clear_bit());

        TAKEN.store(false, Ordering::Release);
        cortex_m::interrupt::enable();
    }

    unsafe fn write(bytes: &[u8]) {
        unsafe { (*ENCODER.0.get()).write(bytes, send_bytes) };
    }
}
