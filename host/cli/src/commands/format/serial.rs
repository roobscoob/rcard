use std::io::{Cursor, Read};
use std::path::PathBuf;

use engine::Backend;
use engine::logs::Logs;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::sync::broadcast;
use zip::ZipArchive;

use crate::format::{self, prefix_width};
use crate::tfw;

/// Base address of RAM on SF32LB52x.
const RAM_BASE: u32 = 0x2000_0000;

/// Number of bytes per write chunk (256 bytes = 64 words).
const CHUNK_SIZE: usize = 65536;

pub async fn run(serial: serial::Serial, firmware: &PathBuf) {
    let meta = tfw::load_metadata_from_bytes(crate::stub::TFW);

    let Some(debug_handle) = serial.debug_handle() else {
        eprintln!(
            "error: the `format` command requires a debug-capable serial backend (USART1 must be connected)"
        );
        std::process::exit(1);
    };

    println!("\x1b[1mFormatting device via serial for firmware {firmware:?}:\x1b[0m");

    // Extract just the firmware partition from the embedded sdmmc image.
    let stub_bytes = crate::stub::TFW;
    let mut archive =
        ZipArchive::new(Cursor::new(stub_bytes)).expect("embedded stub tfw is not a valid archive");

    // Read the partition layout to find the firmware region.
    let layout: serde_json::Value = {
        let mut entry = archive
            .by_name("layout.json")
            .expect("stub tfw missing layout.json");
        let mut json = String::new();
        entry
            .read_to_string(&mut json)
            .expect("failed to read layout.json from stub tfw");
        serde_json::from_str(&json).expect("failed to parse layout.json")
    };

    let fw_part = layout["devices"]
        .as_object()
        .expect("layout missing devices")
        .values()
        .flat_map(|devparts| devparts.as_array().unwrap())
        .find(|p| p["name"] == "firmware")
        .expect("layout missing firmware partition");
    let fw_offset = fw_part["offset_bytes"].as_u64().unwrap() as usize;
    let fw_size = fw_part["size_bytes"].as_u64().unwrap() as usize;

    let mut entry = archive
        .by_name("sdmmc.img")
        .expect("stub tfw missing sdmmc.img");
    let mut full_image = Vec::new();
    entry
        .read_to_end(&mut full_image)
        .expect("failed to read sdmmc.img from stub tfw");
    drop(entry);

    let mut image = full_image[fw_offset..fw_offset + fw_size].to_vec();

    // Pad to a 4-byte boundary for u32 writes.
    while image.len() % 4 != 0 {
        image.push(0);
    }

    // Wait for the device to be alive before proceeding.
    let logs = serial.logs();
    let mut usart1_rx = logs.subscribe_usart1();
    println!("  waiting for device...");
    wait_for_sfbl(&mut usart1_rx).await;

    println!("  entering debug mode...");
    debug_handle
        .enter()
        .await
        .expect("failed to enter debug mode");

    // Halt the core before writing.
    // DHCSR: DBGKEY | C_HALT | C_DEBUGEN
    debug_handle
        .mem_write(0xE000_EDF0, &[0xA05F_0003])
        .await
        .expect("failed to halt core");

    // Write the firmware into RAM.
    let bar = ProgressBar::new(image.len() as u64);
    bar.set_style(
        ProgressStyle::with_template("  writing firmware: [{bar:40}] {bytes}/{total_bytes}")
            .unwrap()
            .progress_chars("=> "),
    );

    for (i, chunk) in image.chunks(CHUNK_SIZE).enumerate() {
        let addr = RAM_BASE + (i * CHUNK_SIZE) as u32;
        let words: Vec<u32> = chunk
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        debug_handle
            .mem_write(addr, &words)
            .await
            .expect("mem_write failed");
        bar.inc(chunk.len() as u64);
    }

    bar.finish_with_message("done");
    println!("  firmware loaded ({} bytes)", image.len());

    // Point VTOR at RAM so the core uses our vector table.
    debug_handle
        .mem_write(0xE000_ED08, &[RAM_BASE])
        .await
        .expect("failed to set VTOR");

    // Read initial SP and reset vector from the vector table in RAM.
    let vtor = debug_handle
        .mem_read(RAM_BASE, 2)
        .await
        .expect("failed to read vector table");
    let initial_sp = vtor[0];
    let reset_vector = vtor[1];

    // Write SP (register 13) via DCRDR/DCRSR.
    debug_handle
        .mem_write(0xE000_EDF8, &[initial_sp])
        .await
        .expect("failed to write DCRDR (SP)");
    // DCRSR: write bit (16) | register 13 (SP)
    debug_handle
        .mem_write(0xE000_EDF4, &[0x0001_000D])
        .await
        .expect("failed to write DCRSR (SP)");

    // Write PC (register 15) via DCRDR/DCRSR.
    debug_handle
        .mem_write(0xE000_EDF8, &[reset_vector])
        .await
        .expect("failed to write DCRDR (PC)");
    // DCRSR: write bit (16) | register 15 (PC)
    debug_handle
        .mem_write(0xE000_EDF4, &[0x0001_000F])
        .await
        .expect("failed to write DCRSR (PC)");

    // Resume execution.
    // DHCSR: DBGKEY | C_DEBUGEN (clear C_HALT)
    println!("  jumping to {reset_vector:#010x} (SP={initial_sp:#010x})...");
    debug_handle
        .mem_write(0xE000_EDF0, &[0xA05F_0001])
        .await
        .expect("failed to resume core");

    let mut structured_rx = logs.subscribe_structured();

    // Tail logs from both USARTs.
    let task_pad = meta
        .task_names
        .iter()
        .map(|n| n.strip_prefix("sysmodule_").unwrap_or(n).len())
        .max()
        .unwrap_or(0)
        .max(6);
    let pw = prefix_width(task_pad);

    loop {
        tokio::select! {
            Ok(entry) = structured_rx.recv() => {
                format::print_structured(&entry, &meta, task_pad, pw);
            }
            Ok(line) = usart1_rx.recv() => {
                format::print_text_line("usart1", &line.text, task_pad);
            }
            else => break,
        }
    }
}

async fn wait_for_sfbl(rx: &mut broadcast::Receiver<engine::logs::Usart1Line>) {
    // First, wait for any SFBL line.
    loop {
        match rx.recv().await {
            Ok(line) if line.text.starts_with("SFBL") => {
                println!("  bootloader: {}", line.text);
                break;
            }
            Ok(_) => {}
            Err(_) => {
                eprintln!("error: USART1 stream closed while waiting for bootloader");
                std::process::exit(1);
            }
        }
    }

    // Now wait for 200ms of silence. If another SFBL arrives, reset the timer.
    // If a non-SFBL line arrives, that's unexpected — bail.
    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(1000), rx.recv()).await {
            Err(_elapsed) => {
                // 200ms of silence — bootloader has stabilized.
                println!("  bootloader stable");
                return;
            }
            Ok(Ok(line)) if line.text.starts_with("SFBL") => {
                // Another reset — keep waiting.
                println!("  bootloader reset: {}", line.text);
                continue;
            }
            Ok(Ok(line)) => {
                eprintln!(
                    "error: unexpected output during bootloader settle: {}",
                    line.text
                );
                std::process::exit(1);
            }
            Ok(Err(_)) => {
                eprintln!("error: USART1 stream closed while waiting for bootloader to settle");
                std::process::exit(1);
            }
        }
    }
}
