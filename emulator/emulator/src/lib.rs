use std::io::Read;
use std::net::TcpStream;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Duration;

mod builder;
mod monitor;
pub mod peripherals;

pub use builder::DeviceBuilder;

use monitor::Monitor;
use peripherals::usart::UsartSink;

/// Flash-mapped base address where the SDMMC image is loaded.
const SDMMC_MAPPING: u64 = 0x1000_0000;

/// Offset of ftab[3] (boot target) within sec_configuration.
/// ftab[0] starts at byte 4; each entry is 16 bytes; index 3 → 4 + 3*16 = 52.
const FTAB_ENTRY3_OFFSET: u64 = 4 + 3 * 16;

/// Magic value at byte 0 of a valid sec_configuration partition.
const SEC_CONFIG_MAGIC: u32 = 0x5345_4346;

#[derive(Debug)]
pub enum EmulatorError {
    RenodeSpawn(std::io::Error),
    MonitorConnect(std::io::Error),
    MonitorSend(std::io::Error),
    MonitorDisconnected,
    MonitorCommand(String),
    TempFile(std::io::Error),
    InvalidFtab(String),
}

impl std::fmt::Display for EmulatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RenodeSpawn(e) => write!(f, "failed to spawn Renode: {e}"),
            Self::MonitorConnect(e) => write!(f, "failed to connect to Renode monitor: {e}"),
            Self::MonitorSend(e) => write!(f, "monitor I/O error: {e}"),
            Self::MonitorDisconnected => write!(f, "Renode monitor disconnected"),
            Self::MonitorCommand(s) => write!(f, "monitor command error: {s}"),
            Self::TempFile(e) => write!(f, "temp file error: {e}"),
            Self::InvalidFtab(s) => write!(f, "invalid ftab: {s}"),
        }
    }
}

impl std::error::Error for EmulatorError {}

pub struct Device {
    renode: std::process::Child,
    monitor: Monitor,
    _usart_threads: Vec<JoinHandle<()>>,
    _temp_dir: tempfile::TempDir,
    temp_path: PathBuf,
    sdmmc_data: Option<Vec<u8>>,
}

impl Device {
    /// Load an SDMMC disk image into emulated flash and the SDMMC peripheral.
    pub fn map_sdmmc(&mut self, data: &[u8]) -> Result<(), EmulatorError> {
        let bin_path = self.temp_path.join("sdmmc.bin");
        std::fs::write(&bin_path, data).map_err(EmulatorError::TempFile)?;
        let path_str = bin_path.to_string_lossy().replace('\\', "/");

        self.sdmmc_data = Some(data.to_vec());

        // Load into flash (for firmware code/ftab)
        self.monitor.query_with_timeout(
            &format!("sysbus LoadBinary @{path_str} 0x{SDMMC_MAPPING:X}"),
            Duration::from_secs(30),
        )?;

        // Load into the SDMMC peripheral's backing storage (for block I/O)
        self.monitor.query_with_timeout(
            &format!("sdmmc1 LoadImage @{path_str}"),
            Duration::from_secs(30),
        )?;

        Ok(())
    }

    /// Start execution. Reads the ftab at 0x1000_0000 to discover the boot
    /// target address, sets VTOR, and runs until the CPU halts.
    pub fn run(&mut self) -> Result<(), EmulatorError> {
        let boot_addr = self.read_ftab_boot_target()?;

        self.monitor
            .send(&format!("cpu VectorTableOffset 0x{boot_addr:X}"))?;
        self.monitor.send("start")?;

        loop {
            std::thread::sleep(Duration::from_millis(500));
            let resp = self.monitor.query("cpu IsHalted")?;
            match resp.as_str() {
                "True" => break,
                "" | "False" => continue,
                other => panic!("unexpected response to 'cpu IsHalted': {:?}", other),
            }
        }
        Ok(())
    }

    /// Parse the ftab at SDMMC_MAPPING to find the boot target address.
    ///
    /// Reads from the in-memory copy of the SDMMC image (set by map_sdmmc).
    /// ftab[3].base is the flash address of the boot target.
    fn read_ftab_boot_target(&self) -> Result<u64, EmulatorError> {
        let data = self.sdmmc_data.as_ref().ok_or_else(|| {
            EmulatorError::InvalidFtab("no SDMMC image loaded (call map_sdmmc first)".into())
        })?;

        if data.len() < 4 {
            return Err(EmulatorError::InvalidFtab("image too small".into()));
        }

        // Verify magic
        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if magic != SEC_CONFIG_MAGIC {
            return Err(EmulatorError::InvalidFtab(format!(
                "expected magic 0x{SEC_CONFIG_MAGIC:08X}, got 0x{magic:08X}"
            )));
        }

        // Read ftab[3].base (first u32 of the entry)
        let off = FTAB_ENTRY3_OFFSET as usize;
        if data.len() < off + 4 {
            return Err(EmulatorError::InvalidFtab(
                "image too small for ftab[3]".into(),
            ));
        }
        let boot_addr = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);

        if boot_addr == 0 || boot_addr == 0xFFFF_FFFF {
            return Err(EmulatorError::InvalidFtab(format!(
                "ftab[3].base is 0x{boot_addr:08X} (uninitialized)"
            )));
        }

        Ok(boot_addr as u64)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.monitor.send("quit").ok();
        // Give it a moment, then force kill
        std::thread::sleep(Duration::from_millis(500));
        self.renode.kill().ok();
        self.renode.wait().ok();
    }
}

/// Spawn a background thread that reads bytes from a TCP stream
/// and feeds them to a UsartSink.
fn spawn_usart_reader(port: u16, mut sink: impl UsartSink) -> JoinHandle<()> {
    std::thread::spawn(move || {
        // Retry connecting — Renode's socket terminal may not be ready yet
        let stream = {
            let addr = format!("127.0.0.1:{port}");
            let mut attempts = 0;
            loop {
                match TcpStream::connect(&addr) {
                    Ok(s) => break s,
                    Err(_) => {
                        attempts += 1;
                        if attempts > 50 {
                            eprintln!("failed to connect to USART socket on port {port}");
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        };

        let mut buf = [0u8; 256];
        let mut reader = std::io::BufReader::new(stream);
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for &byte in &buf[..n] {
                        sink.on_byte(byte);
                    }
                }
                Err(_) => break,
            }
        }
    })
}

fn find_free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
