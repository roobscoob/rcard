#![no_std]
#![no_main]

use core::mem::MaybeUninit;

use hubris_task_slots::SLOTS;
use sysmodule_log_api::{Log, LogDispatcher};

sysmodule_usart_api::bind!(Usart = SLOTS.sysmodule_usart);

static mut USART: MaybeUninit<Usart> = MaybeUninit::uninit();

struct LogResource;

impl Log for LogResource {
    fn write(
        _meta: ipc::Meta,
        data: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) {
        let usart = unsafe { USART.assume_init_ref() };
        let len = data.len();
        let mut buf = [0u8; 128];
        let mut offset = 0;
        while offset < len {
            let chunk = (len - offset).min(buf.len());
            let _ = data.read_range(offset, &mut buf[..chunk]);
            let _ = usart.write(&buf[..chunk]);
            offset += chunk;
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    // Open USART2 for log output.
    let usart = Usart::open(2).unwrap().unwrap();
    unsafe { USART.write(usart) };

    let mut dispatcher = LogDispatcher::<LogResource>::new();
    let mut buf = [MaybeUninit::uninit(); 256];

    ipc::Server::<1>::new()
        .with_dispatcher(0x03, &mut dispatcher)
        .run(&mut buf)
}
