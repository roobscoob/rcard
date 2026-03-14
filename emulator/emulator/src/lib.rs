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

#[derive(Debug)]
pub enum EmulatorError {
    RenodeSpawn(std::io::Error),
    MonitorConnect(std::io::Error),
    MonitorSend(std::io::Error),
    MonitorDisconnected,
    MonitorCommand(String),
    TempFile(std::io::Error),
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
}

impl Device {
    pub fn load_binary(&mut self, addr: u64, data: &[u8]) -> Result<(), EmulatorError> {
        let bin_path = self.temp_path.join("firmware.bin");
        std::fs::write(&bin_path, data).map_err(EmulatorError::TempFile)?;
        let path_str = bin_path.to_string_lossy().replace('\\', "/");

        self.monitor
            .send(&format!("sysbus LoadBinary @{path_str} 0x{addr:X}"))
    }

    pub fn run(&mut self, vtor: u64, _until: u64) -> Result<(), EmulatorError> {
        self.monitor
            .send(&format!("cpu VectorTableOffset 0x{vtor:X}"))?;
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
