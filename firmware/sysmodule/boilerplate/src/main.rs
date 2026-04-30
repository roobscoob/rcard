#![no_std]
#![no_main]

use generated::slots::SLOTS;
use sysmodule_boilerplate_api::*;

// ── Logging ─────────────────────────────────────────────────────────
sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

// ── Entry point ─────────────────────────────────────────────────────

struct DemoServer;

impl Demo for DemoServer {
    fn hello(_meta: ipc::Meta) {
        rcard_log::info!("Hello World");
    }
}

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("Awake");

    ipc::server! {
        Demo: DemoServer
    }
}
