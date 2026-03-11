use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::monitor::Monitor;
use crate::peripherals::usart::log::UsartLog;
use crate::peripherals::usart::{NullSink, StringLogger, StructuredSink};
use crate::{find_free_port, spawn_usart_reader, Device, EmulatorError};

const RENODE_EXE: &str = r"C:\Program Files\Renode\bin\Renode.exe";

pub struct DeviceBuilder {
    log_tx: Option<mpsc::Sender<UsartLog>>,
    renode_assets: Option<std::path::PathBuf>,
}

impl DeviceBuilder {
    pub fn new() -> Self {
        DeviceBuilder {
            log_tx: None,
            renode_assets: None,
        }
    }

    pub fn with_logger(mut self, tx: mpsc::Sender<UsartLog>) -> Self {
        self.log_tx = Some(tx);
        self
    }

    /// Path to the directory containing .repl and .cs files (firmware/renode/).
    pub fn with_renode_assets(mut self, path: std::path::PathBuf) -> Self {
        self.renode_assets = Some(path);
        self
    }

    pub fn build(self) -> Result<Device, EmulatorError> {
        let temp_dir = tempfile::tempdir().map_err(EmulatorError::TempFile)?;
        let temp_path = temp_dir.path().to_path_buf();

        let monitor_port = find_free_port();
        let usart1_port = find_free_port();
        let usart2_port = find_free_port();
        let usart3_port = find_free_port();

        // Resolve asset path
        let assets = self.renode_assets.unwrap_or_else(|| {
            // Default: look relative to the workspace
            let mut p = std::env::current_dir().unwrap();
            p.push("firmware");
            p.push("renode");
            p
        });
        let assets_str = assets.to_string_lossy().replace('\\', "/");

        // Generate .resc script
        let resc = format!(
            r#"using sysbus

i @{assets_str}/SSD1312.cs
i @{assets_str}/SF32LB52_RTC.cs
i @{assets_str}/SF32LB52_SDMMC.cs

mach create "sf32lb52"
machine LoadPlatformDescription @{assets_str}/sf32lb52.repl

emulation CreateServerSocketTerminal {usart1_port} "usart1_term" false
connector Connect usart1 usart1_term

emulation CreateServerSocketTerminal {usart2_port} "usart2_term" false
connector Connect usart2 usart2_term

emulation CreateServerSocketTerminal {usart3_port} "usart3_term" false
connector Connect usart3 usart3_term

logLevel 3 nvic
"#
        );

        let resc_path = temp_path.join("emulator.resc");
        std::fs::write(&resc_path, &resc).map_err(EmulatorError::TempFile)?;
        let resc_str = resc_path.to_string_lossy().replace('\\', "/");

        println!("Generated Renode script:\n{resc_str}");

        // Spawn Renode headless
        let renode = Command::new(RENODE_EXE)
            .args([
                "--disable-xwt",
                "--port",
                &monitor_port.to_string(),
                "--execute",
                &format!("include @{resc_str}"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(EmulatorError::RenodeSpawn)?;

        // Connect to monitor (retry up to 15s for Renode startup)
        let monitor = Monitor::connect(monitor_port, Duration::from_secs(15))?;

        // Spawn USART reader threads
        let mut usart_threads = Vec::new();

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
        })
    }
}
