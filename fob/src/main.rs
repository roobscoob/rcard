#![no_std]
#![no_main]

use hubris_task_slots::SLOTS;
use sysmodule_display_api::DisplayConfiguration;

sysmodule_display_api::bind_display!(Display = SLOTS.sysmodule_display);
sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
sysmodule_log_api::panic_handler!(to Log; cleanup Display, StoragePartition, Fs, FsFile);
sysmodule_storage_api::bind_partition!(StoragePartition = SLOTS.sysmodule_storage);
sysmodule_fs_api::bind_file_system!(Fs = SLOTS.sysmodule_fs);
sysmodule_fs_api::bind_file!(FsFile = SLOTS.sysmodule_fs);

#[export_name = "main"]
fn main() -> ! {
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
    let _read = file.read(sysmodule_fs_api::FileOffset::new(0).unwrap(), &mut buf[..size as usize]).unwrap();
    file.close().unwrap();

    loop {
        for _ in 0..100_000_000 {
            core::hint::spin_loop();
        }
    }
}
