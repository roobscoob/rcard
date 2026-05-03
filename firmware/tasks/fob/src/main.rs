#![no_std]
#![no_main]

use generated::slots::SLOTS;
use rcard_log::info;
use rcard_log::ResultExt;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);
sysmodule_lcpu_api::bind_lcpu!(Lcpu = SLOTS.sysmodule_lcpu);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

#[ipc::notification_handler(lcpu_data)]
fn handle_lcpu_data(_sender: u16, _code: u32) {
    info!("got the lcpu data uhhh");
}

#[export_name = "main"]
fn main() -> ! {
    info!("Awake");

    let _lcpu = Lcpu::init([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]).log_unwrap().log_unwrap();
    
    info!("lcpu inited???!?! woaw!");

    ipc::server! {
        @notifications(Reactor) => handle_lcpu_data,
    }
}
