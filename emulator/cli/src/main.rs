use std::sync::mpsc;

use emulator::peripherals::usart::log::{UsartLog, UsartLogKind};
use emulator::peripherals::usart::HexDumpSink;
use emulator::DeviceBuilder;

fn main() {
    let bin_path = std::env::args().nth(1).expect("usage: emulator-cli <binary>");
    let bin = std::fs::read(&bin_path).expect("failed to read binary");

    // Derive renode assets path from binary: <firmware>/build/x.bin → <firmware>/renode/
    let bin_abs = std::fs::canonicalize(&bin_path).expect("failed to resolve binary path");
    let firmware_dir = bin_abs
        .parent() // build/
        .and_then(|p| p.parent()) // firmware/
        .expect("binary must be inside firmware/build/");
    let assets = firmware_dir.join("renode");

    let (tx, rx) = mpsc::channel::<UsartLog>();

    let mut device = DeviceBuilder::new()
        .with_logger(tx)
        .with_renode_assets(assets)
        .build()
        .expect("failed to start emulator");

    let load_addr = 0x2002_0000u64;
    device
        .load_binary(load_addr, &bin)
        .expect("failed to load binary");

    let log_thread = std::thread::spawn(move || {
        for log in rx {
            match log.kind {
                UsartLogKind::Line(line) => {
                    println!("[USART{}] {}", log.channel, line);
                }
                UsartLogKind::Stream(stream) => {
                    println!(
                        "[USART{}] stream: {:?} level={:?} t={}",
                        log.channel,
                        stream.metadata.log_id,
                        stream.metadata.level,
                        stream.metadata.timestamp,
                    );
                    for value in stream.values {
                        println!("  {:?}", value);
                    }
                }
            }
        }
    });

    match device.run(load_addr, 0) {
        Ok(()) => println!("emulation finished"),
        Err(e) => eprintln!("emulation error: {:?}", e),
    }

    drop(device);
    let _ = log_thread.join();
}
