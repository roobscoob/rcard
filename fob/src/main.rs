#![no_std]
#![no_main]

use hubris_task_slots::SLOTS;
use sysmodule_display_api::DisplayConfiguration;
use sysmodule_storage_api::partitions;

sysmodule_display_api::bind_display!(Display = SLOTS.sysmodule_display);
sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
sysmodule_log_api::panic_handler!(Log);
sysmodule_storage_api::bind_partition!(StoragePartition = SLOTS.sysmodule_storage);
sysmodule_fs_api::bind_file_system!(Fs = SLOTS.sysmodule_fs);
sysmodule_fs_api::bind_file!(FsFile = SLOTS.sysmodule_fs);

#[export_name = "main"]
fn main() -> ! {
    sysmodule_log_api::init_logger!(Log);

    let display = DisplayConfiguration::builder(128, 64)
        .build()
        .open::<DisplayServer>()
        .unwrap()
        .unwrap();

    // 1x1 pixel checkerboard
    let mut fb = [0u8; 128 * 8];
    for page in 0..8usize {
        for col in 0..128usize {
            fb[page * 128 + col] = if col & 1 == 0 { 0x55 } else { 0xAA };
        }
    }

    display.draw(&fb).unwrap();

    // files :)
    let buf = &mut [0u8; 128];
    let file = FsFile::get(b"main:/demo.txt").unwrap().unwrap();
    let size = file.size().unwrap();
    let read = file.read(0, &mut buf[..size as usize]).unwrap();
    file.close().unwrap();

    log::info!(
        "Read {} bytes from demo.txt: {:?}",
        read,
        core::str::from_utf8(&buf[..read as usize]).unwrap_or("<invalid utf8>")
    );

    loop {
        // log::info!("tick");

        // for _ in 0..100_000_000 {
        //     core::hint::spin_loop();
        // }
    }
}
