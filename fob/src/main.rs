#![no_std]
#![no_main]

use hubris_task_slots::SLOTS;
use sysmodule_display_api::DisplayConfiguration;

sysmodule_display_api::bind_display!(Display = SLOTS.sysmodule_display);
sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
sysmodule_log_api::panic_handler!(Log);
sysmodule_sdmmc_api::bind_sdmmc!(Sdmmc = SLOTS.sysmodule_sdmmc);
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

    log::info!("checkerboard drawing");
    display.draw(&fb).unwrap();

    // ── Open SD card ──
    log::info!("opening sdmmc...");
    let sdmmc = Sdmmc::open().unwrap().unwrap();

    log::info!("sdmmc opened successfully");

    let blocks = sdmmc.block_count().unwrap();
    log::info!("sdmmc: {} blocks", blocks);

    // ── Format and mount filesystem ──
    log::info!("formatting filesystem...");
    let fs = Fs::format(sdmmc).unwrap().unwrap();
    log::info!("filesystem mounted");

    // ── Write a file ──
    let message = b"Hello from Hubris!";
    log::info!("creating /hello.txt ...");
    let file = FsFile::get_or_create(&fs, b"/hello.txt").unwrap().unwrap();
    let written = file.write(0, message).unwrap();
    log::info!("wrote {} bytes", written);
    file.close().unwrap();

    // ── Read it back ──
    log::info!("reading /hello.txt ...");
    let file = FsFile::get(&fs, b"/hello.txt").unwrap().unwrap();
    let size = file.size().unwrap();
    log::info!("file size: {} bytes", size);

    let mut buf = [0u8; 64];
    let read = file.read(0, &mut buf[..size as usize]).unwrap();
    file.close().unwrap();

    if &buf[..read as usize] == message {
        log::info!("fs test PASSED: read back matches");
    } else {
        log::error!("fs test FAILED: readback mismatch");
    }

    loop {
        log::info!("tick");
        for _ in 0..100_000_000 {
            core::hint::spin_loop();
        }
    }
}
