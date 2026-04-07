#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use sifli_pac::usart::{vals::M, Usart as UsartPeri};

use sysmodule_usart_api::*;

static USART_IN_USE: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

fn usart_instance(index: u8) -> Option<UsartPeri> {
    match index {
        2 => Some(sifli_pac::USART2),
        3 => Some(sifli_pac::USART3),
        _ => None,
    }
}

fn init_usart(regs: UsartPeri) {
    // BRR = 48MHz / 921600 = 52 (0x34)
    regs.brr().write(|w| w.0 = 0x34);
    regs.cr1().write(|w| {
        w.set_m(M::Bit8);
        w.set_ue(true);
        w.set_te(true);
    });
}

struct UsartResource {
    index: u8,
    regs: UsartPeri,
}

impl Usart for UsartResource {
    fn open(_meta: ipc::Meta, index: u8) -> Result<Self, UsartOpenError> {
        if index == 1 {
            return Err(UsartOpenError::ReservedUsart);
        }

        let Some(regs) = usart_instance(index) else {
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
            while !self.regs.isr().read().txe() {}
            self.regs.tdr().write(|w| w.0 = b as u32);
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
