#![no_std]
#![no_main]

use generated::slots::SLOTS;
// use rcard_log::{error, warn, OptionExt, ResultExt};
use sysmodule_lcpu_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

mod memory_map;
mod nvds;
mod lcpu;

struct LcpuResource {

}

impl Lcpu for LcpuResource {
    fn init(_meta: ipc::Meta) -> Result<Self,()> {
        todo!()
    }

    fn send_data(_meta: ipc::Meta, _data: ipc::dispatch::LeaseBorrow<'_,ipc::dispatch::Read>) -> () {
        todo!()
    }
}

impl Drop for LcpuResource {
    fn drop(&mut self) -> () {
        todo!()
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    ipc::server! {
        Lcpu: LcpuResource,
    }
}
