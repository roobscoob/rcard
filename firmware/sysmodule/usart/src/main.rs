#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use sf32lb52_pac::usart1::RegisterBlock;
use sf32lb52_pac::{Usart2, Usart3};

use sysmodule_usart_api::*;

static USART_IN_USE: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

fn usart_register_block(index: u8) -> Option<&'static RegisterBlock> {
    match index {
        2 => Some(unsafe { &*Usart2::PTR }),
        3 => Some(unsafe { &*Usart3::PTR }),
        _ => None,
    }
}

fn init_usart(regs: &RegisterBlock) {
    // BRR = 48MHz / 115200 = 417 (0x1A1)
    regs.brr().write(|w| unsafe { w.bits(0x1A1) });
    regs.cr1().write(|w| w.ue().set_bit().te().set_bit());
}

struct UsartResource {
    index: u8,
    regs: &'static RegisterBlock,
}

impl Usart for UsartResource {
    fn open(_meta: ipc::Meta, index: u8) -> Result<Self, UsartOpenError> {
        if index == 1 {
            return Err(UsartOpenError::ReservedUsart);
        }

        let Some(regs) = usart_register_block(index) else {
            return Err(UsartOpenError::InvalidIndex);
        };

        if USART_IN_USE[(index - 2) as usize].swap(true, Ordering::Acquire) {
            return Err(UsartOpenError::AlreadyOpen);
        }

        init_usart(regs);

        Ok(UsartResource { index, regs })
    }

    fn write(
        &mut self,
        _meta: ipc::Meta,
        data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) {
        for i in 0..data.len() {
            let b = data.read(i).unwrap_or(0);
            while self.regs.isr().read().txe().bit_is_clear() {}
            self.regs.tdr().write(|w| unsafe { w.bits(b as u32) });
        }
    }
}

impl Drop for UsartResource {
    fn drop(&mut self) {
        USART_IN_USE[(self.index - 2) as usize].store(false, Ordering::Release);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    userlib::sys_panic(b"usart panic")
}

#[export_name = "main"]
fn main() -> ! {
    ipc::server! {
        Usart: UsartResource,
    }
}
