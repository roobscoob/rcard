use std::io::BufRead;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::monitor::Monitor;
use crate::peripherals::usart::log::{UsartLog, UsartLogKind};
use crate::peripherals::usart::{NullSink, StringLogger, StructuredSink};
use crate::{Device, EmulatorError, FLASH_BASE, find_free_port, spawn_usart_reader};

const RENODE_EXE: &str = r"C:\Program Files\Renode\bin\Renode.exe";

const MODEL_SSD1312: &str = include_str!("models/SSD1312.cs");
const MODEL_SF32LB52_RTC: &str = include_str!("models/SF32LB52_RTC.cs");
const MODEL_SF32LB52_SDMMC: &str = include_str!("models/SF32LB52_SDMMC.cs");
const MODEL_SF32LB52_MPI: &str = include_str!("models/SF32LB52_MPI.cs");

pub struct DeviceBuilder {
    log_tx: Option<mpsc::Sender<UsartLog>>,
    renode_platform: Option<String>,
    flash_image: Option<Vec<u8>>,
}

impl DeviceBuilder {
    pub fn new() -> Self {
        DeviceBuilder {
            log_tx: None,
            renode_platform: None,
            flash_image: None,
        }
    }

    pub fn with_logger(mut self, tx: mpsc::Sender<UsartLog>) -> Self {
        self.log_tx = Some(tx);
        self
    }

    /// Renode platform description (.repl contents). Typically extracted from
    /// the `renode_def.repl` entry in a `.tfw` archive.
    pub fn with_platform(mut self, repl: String) -> Self {
        self.renode_platform = Some(repl);
        self
    }

    pub fn with_flash(mut self, data: Vec<u8>) -> Self {
        self.flash_image = Some(data);
        self
    }

    pub fn build(self) -> Result<Device, EmulatorError> {
        let temp_dir = tempfile::tempdir().map_err(EmulatorError::TempFile)?;
        let temp_path = temp_dir.path().to_path_buf();

        let monitor_port = find_free_port();
        let usart1_port = find_free_port();
        let usart2_port = find_free_port();
        let usart3_port = find_free_port();

        let repl = self.renode_platform.ok_or_else(|| {
            EmulatorError::InvalidFtab(
                "no platform description provided (call with_platform)".into(),
            )
        })?;

        let flash_image = self.flash_image.ok_or_else(|| {
            EmulatorError::InvalidFtab("no flash image provided (call with_flash)".into())
        })?;

        // Write assets to temp dir (Renode can't handle spaces in paths)
        let assets_str = temp_path.to_string_lossy().replace('\\', "/");
        for (name, content) in [
            ("SSD1312.cs", MODEL_SSD1312),
            ("SF32LB52_RTC.cs", MODEL_SF32LB52_RTC),
            ("SF32LB52_SDMMC.cs", MODEL_SF32LB52_SDMMC),
            ("SF32LB52_MPI.cs", MODEL_SF32LB52_MPI),
            ("sf32lb52.repl", &repl),
        ] {
            std::fs::write(temp_path.join(name), content).map_err(EmulatorError::TempFile)?;
        }
        std::fs::write(temp_path.join("flash.bin"), &flash_image)
            .map_err(EmulatorError::TempFile)?;

        // Spawn Renode headless (no --execute; we drive everything via monitor)
        let mut renode = Command::new(RENODE_EXE)
            .args(["--disable-xwt", "--port", &monitor_port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(EmulatorError::RenodeSpawn)?;

        // Spawn stdout/stderr reader threads immediately so Renode's output
        // pipe doesn't fill up and block.
        let mut usart_threads = Vec::new();

        for pipe in [
            renode
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            renode
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        ] {
            if let Some(pipe) = pipe {
                let tx = self.log_tx.clone();
                usart_threads.push(std::thread::spawn(move || {
                    let reader = std::io::BufReader::new(pipe);
                    for line in reader.lines() {
                        let line = match line {
                            Ok(l) => l,
                            Err(_) => break,
                        };
                        match &tx {
                            Some(tx) => {
                                let _ = tx.send(UsartLog {
                                    channel: 0,
                                    kind: UsartLogKind::Renode(line),
                                });
                            }
                            None => eprintln!("[renode] {line}"),
                        }
                    }
                }));
            }
        }

        // Connect to monitor
        let mut monitor = Monitor::connect(monitor_port, Duration::from_secs(15))?;

        // Send all setup commands sequentially via monitor — errors come back
        // as responses so we can see them in the log.
        monitor.send(&format!("i @{assets_str}/SSD1312.cs"))?;
        monitor.send(&format!("i @{assets_str}/SF32LB52_RTC.cs"))?;
        monitor.send(&format!("i @{assets_str}/SF32LB52_SDMMC.cs"))?;
        monitor.send(&format!("i @{assets_str}/SF32LB52_MPI.cs"))?;
        monitor.send("mach create \"sf32lb52\"")?;
        let resp = monitor.query(&format!(
            "machine LoadPlatformDescription @{assets_str}/sf32lb52.repl",
        ))?;
        if !resp.is_empty() {
            return Err(EmulatorError::MonitorCommand(resp));
        }
        monitor.send(&format!(
            "sysbus LoadBinary @{assets_str}/flash.bin 0x{FLASH_BASE:X}"
        ))?;
        monitor.send(&format!(
            "emulation CreateServerSocketTerminal {usart1_port} \"usart1_term\" false",
        ))?;
        monitor.send("connector Connect usart1 usart1_term")?;
        monitor.send(&format!(
            "emulation CreateServerSocketTerminal {usart2_port} \"usart2_term\" false",
        ))?;
        monitor.send("connector Connect usart2 usart2_term")?;
        monitor.send(&format!(
            "emulation CreateServerSocketTerminal {usart3_port} \"usart3_term\" false",
        ))?;
        monitor.send("connector Connect usart3 usart3_term")?;
        monitor.send("logLevel 3 nvic")?;

        // Spawn USART reader threads
        match &self.log_tx {
            Some(tx) => {
                usart_threads.push(spawn_usart_reader(
                    usart1_port,
                    StringLogger::new(1, tx.clone()),
                ));
                usart_threads.push(spawn_usart_reader(
                    usart2_port,
                    StructuredSink::new(2, tx.clone()),
                ));
            }
            None => {
                usart_threads.push(spawn_usart_reader(usart1_port, NullSink));
                usart_threads.push(spawn_usart_reader(usart2_port, NullSink));
            }
        }
        usart_threads.push(spawn_usart_reader(usart3_port, NullSink));

        Ok(Device {
            renode,
            monitor,
            _usart_threads: usart_threads,
            temp_path,
            _temp_dir: temp_dir,
            flash_data: Some(flash_image),
        })
    }
}
