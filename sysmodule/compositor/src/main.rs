#![no_std]
#![no_main]

pub mod frame_buffers;

use hubris_task_slots::SLOTS;
use sysmodule_compositor_api::*;
use sysmodule_log_api::log;

sysmodule_display_api::bind_display!(Display = SLOTS.sysmodule_display);
sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
sysmodule_log_api::panic_handler!(to Log; cleanup Display);

struct CompositorImpl;

impl Compositor for CompositorImpl {
    fn ping(_meta: ipc::Meta) -> u32 {
        10
    }
}

#[export_name = "main"]
fn main() -> ! {
    sysmodule_log_api::init_logger!(Log);

    log::info!("compositor starting up");

    ipc::server! {
        Compositor: CompositorImpl,
    }
}
