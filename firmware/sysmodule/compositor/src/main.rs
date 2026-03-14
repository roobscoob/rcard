#![no_std]
#![no_main]

pub mod frame_buffers;

use hubris_task_slots::SLOTS;
use sysmodule_compositor_api::*;

sysmodule_display_api::bind_display!(Display = SLOTS.sysmodule_display);
sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log; cleanup Display);

struct CompositorImpl;

impl Compositor for CompositorImpl {
    fn ping(_meta: ipc::Meta) -> u32 {
        10
    }
}

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("Awake");
    ipc::server! {
        Compositor: CompositorImpl,
    }
}
