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
    rcard_log::info!("Testing\nNewlines!");

    // test the range of log levels
    rcard_log::trace!("trace message");
    rcard_log::debug!("debug message");
    rcard_log::info!("info message");
    rcard_log::warn!("warn message");
    rcard_log::error!("error message");
    rcard_log::panic!("panic message");

    // loop {
    //     core::hint::spin_loop();
    // }
}
