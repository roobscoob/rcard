use std::io::BufRead;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::monitor::Monitor;
use crate::peripherals::usart::log::{UsartLog, UsartLogKind};
use crate::peripherals::usart::{NullSink, StringLogger, StructuredSink};
use crate::{Device, EmulatorError, find_free_port, spawn_usart_reader};

fn find_renode() -> Result<std::path::PathBuf, EmulatorError> {
    which::which("renode").or_else(|_| which::which("Renode")).map_err(|_| {
        EmulatorError::RenodeSpawn(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "could not find Renode on PATH",
        ))
    })
}

const MODEL_SSD1312: &str = include_str!("models/SSD1312.cs");
const MODEL_SF32LB52_RTC: &str = include_str!("models/SF32LB52_RTC.cs");
const MODEL_SF32LB52_SDMMC: &str = include_str!("models/SF32LB52_SDMMC.cs");
const MODEL_SF32LB52_MPI: &str = include_str!("models/SF32LB52_MPI.cs");
const MODEL_SF32LB52_HPSYS_RCC: &str = include_str!("models/SF32LB52_HPSYS_RCC.cs");
const MODEL_SF32LB52_HPSYS_AON: &str = include_str!("models/SF32LB52_HPSYS_AON.cs");

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

    pub fn with_places(mut self, data: Vec<u8>) -> Self {
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
            EmulatorError::InvalidPlaces(
                "no platform description provided (call with_platform)".into(),
            )
        })?;

        let places_data = self.flash_image.ok_or_else(|| {
            EmulatorError::InvalidPlaces("no places binary provided (call with_places)".into())
        })?;

        // Write assets to temp dir (Renode can't handle spaces in paths)
        let assets_str = temp_path.to_string_lossy().replace('\\', "/");
        for (name, content) in [
            ("SSD1312.cs", MODEL_SSD1312),
            ("SF32LB52_RTC.cs", MODEL_SF32LB52_RTC),
            ("SF32LB52_SDMMC.cs", MODEL_SF32LB52_SDMMC),
            ("SF32LB52_MPI.cs", MODEL_SF32LB52_MPI),
            ("SF32LB52_HPSYS_RCC.cs", MODEL_SF32LB52_HPSYS_RCC),
            ("SF32LB52_HPSYS_AON.cs", MODEL_SF32LB52_HPSYS_AON),
            ("sf32lb52.repl", &repl),
        ] {
            std::fs::write(temp_path.join(name), content).map_err(EmulatorError::TempFile)?;
        }
        // Places data is stored on the Device for segment loading in run().


        // Spawn Renode headless (no --execute; we drive everything via monitor)
        let mut renode = Command::new(find_renode()?)
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
        let monitor = Monitor::connect(monitor_port, Duration::from_secs(15))?;

        // Send all setup commands sequentially via monitor — errors come back
        // as responses so we can see them in the log.
        for model in [
            "SSD1312.cs",
            "SF32LB52_RTC.cs",
            "SF32LB52_SDMMC.cs",
            "SF32LB52_MPI.cs",
            "SF32LB52_HPSYS_RCC.cs",
            "SF32LB52_HPSYS_AON.cs",
        ] {
            let resp = monitor.query(&format!("i @{assets_str}/{model}"))?;
            if !resp.is_empty() && resp.contains("error") {
                return Err(EmulatorError::MonitorCommand(format!("loading {model}: {resp}")));
            }
        }
        monitor.send("mach create \"sf32lb52\"")?;
        let resp = monitor.query(&format!(
            "machine LoadPlatformDescription @{assets_str}/sf32lb52.repl",
        ))?;
        if !resp.is_empty() {
            return Err(EmulatorError::MonitorCommand(resp));
        }
        // Segments are loaded individually in Device::run() from the places binary.

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
        monitor.send("logLevel 3 usart1")?;
        monitor.send("logLevel 3 usart2")?;

        // Pre-enable USART transmitters (the real bootloader does this)
        monitor.send("usart1 WriteDoubleWord 0x0 0x9")?;
        monitor.send("usart2 WriteDoubleWord 0x0 0x9")?;

        // Spawn USART reader threads
        match &self.log_tx {
            Some(tx) => {
                usart_threads.push(spawn_usart_reader(
                    usart1_port,
                    "usart1",
                    StringLogger::new(1, tx.clone()),
                ));
                usart_threads.push(spawn_usart_reader(
                    usart2_port,
                    "usart2",
                    StructuredSink::new(2, tx.clone()),
                ));
            }
            None => {
                usart_threads.push(spawn_usart_reader(usart1_port, "usart1", NullSink));
                usart_threads.push(spawn_usart_reader(usart2_port, "usart2", NullSink));
            }
        }
        usart_threads.push(spawn_usart_reader(usart3_port, "usart3", NullSink));

        Ok(Device {
            renode,
            monitor,
            _usart_threads: usart_threads,
            temp_path,
            _temp_dir: temp_dir,
            places_data: Some(places_data),
        })
    }
}
