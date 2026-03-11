#![no_std]
#![no_main]

use hubris_task_slots::SLOTS;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("hello from fob!");
    rcard_log::info!("the answer is {}", 42u32);

    loop {
        core::hint::spin_loop();
    }
}
