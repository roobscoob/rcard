#![no_std]
#![no_main]

use hubris_task_slots::SLOTS;
use sysmodule_display_api::DisplayConfiguration;

sysmodule_usart_api::bind!(Usart = SLOTS.sysmodule_usart);
sysmodule_display_api::bind!(Display = SLOTS.sysmodule_display);
sysmodule_log_api::bind!(Log = SLOTS.sysmodule_log);

#[export_name = "main"]
fn main() -> ! {
    let display = DisplayConfiguration::builder(128, 64)
        .build()
        .open::<DisplayServer>()
        .unwrap()
        .unwrap();

    // 1x1 pixel checkerboard: alternating 0x55/0xAA per column
    // 0x55 = 0b01010101 — pixels at rows 0,2,4,6 on
    // 0xAA = 0b10101010 — pixels at rows 1,3,5,7 on
    let mut fb = [0u8; 128 * 8];
    for page in 0..8usize {
        for col in 0..128usize {
            fb[page * 128 + col] = if col & 1 == 0 { 0x55 } else { 0xAA };
        }
    }

    display.draw(&fb).unwrap();
    Log::write(b"[fob] checkerboard drawn\r\n").unwrap();

    loop {}
}
