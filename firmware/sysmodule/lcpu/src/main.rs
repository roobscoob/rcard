#![no_std]
#![no_main]

use generated::slots::SLOTS;
use rcard_log::{error, warn, OptionExt, ResultExt};
use sysmodule_lcpu_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

#[export_name = "main"]
fn main() -> ! {
    ipc::server! {
        // Time: TimeImpl,
    }
}
