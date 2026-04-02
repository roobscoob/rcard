use std::io::{Cursor, Read as _};
use std::path::Path;
use std::sync::mpsc;

use engine::logs::{HypervisorLine, LogEntry, Logs};
use engine::Backend;
use tokio::sync::broadcast;
use zip::ZipArchive;

use crate::peripherals::usart::log::{UsartLog, UsartLogKind};
use crate::{DeviceBuilder, EmulatorError};

/// An emulator session implementing `Backend`.
///
/// Created via `Emulator::start`, which boots the device in Renode.
/// Drop shuts down Renode.
pub struct Emulator {
    _run_thread: std::thread::JoinHandle<()>,
    logs: EmulatorLogs,
}

struct EmulatorLogs {
    structured_tx: broadcast::Sender<LogEntry>,
    hypervisor_tx: broadcast::Sender<HypervisorLine>,
    renode_tx: broadcast::Sender<String>,
}

impl Emulator {
    /// Boot the emulator from a `.tfw` archive.
    ///
    /// Extracts `sdmmc.img` and `renode_platform.repl` from the archive,
    /// then starts Renode and runs the firmware.
    pub fn start(tfw: &Path) -> Result<Self, EmulatorError> {
        let tfw_bytes = std::fs::read(tfw).map_err(EmulatorError::TempFile)?;
        let mut archive = ZipArchive::new(Cursor::new(tfw_bytes))
            .map_err(|e| EmulatorError::InvalidFtab(format!("invalid tfw archive: {e}")))?;

        let sdmmc_image = read_entry(&mut archive, "sdmmc.img")?;
        let platform_repl = String::from_utf8(read_entry(&mut archive, "renode_platform.repl")?)
            .map_err(|e| {
                EmulatorError::InvalidFtab(format!("renode_platform.repl is not valid UTF-8: {e}"))
            })?;

        let (structured_tx, _) = broadcast::channel(256);
        let (hypervisor_tx, _) = broadcast::channel(256);
        let (renode_tx, _) = broadcast::channel(256);

        let s_tx = structured_tx.clone();
        let h_tx = hypervisor_tx.clone();
        let r_tx = renode_tx.clone();

        // Everything — Renode spawn, monitor connect, load, run — happens in the background.
        let run_thread = std::thread::spawn(move || {
            let (log_tx, log_rx) = mpsc::channel();

            // Bridge thread: forwards mpsc logs to broadcast channels.
            std::thread::spawn(move || {
                bridge_logs(log_rx, s_tx, h_tx, r_tx);
            });

            let mut device = match DeviceBuilder::new()
                .with_logger(log_tx)
                .with_platform(platform_repl)
                .with_flash(sdmmc_image)
                .build()
            {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("emulator build error: {e}");
                    return;
                }
            };

            if let Err(e) = device.run() {
                eprintln!("emulator run error: {e}");
            }
        });

        Ok(Emulator {
            _run_thread: run_thread,
            logs: EmulatorLogs {
                structured_tx,
                hypervisor_tx,
                renode_tx,
            },
        })
    }
}

fn read_entry(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    name: &str,
) -> Result<Vec<u8>, EmulatorError> {
    let mut entry = archive
        .by_name(name)
        .map_err(|e| EmulatorError::InvalidFtab(format!("missing {name} in tfw: {e}")))?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry
        .read_to_end(&mut buf)
        .map_err(EmulatorError::TempFile)?;
    Ok(buf)
}

impl Backend for Emulator {
    fn logs(&self) -> &dyn Logs {
        &self.logs
    }
}

const AUX_STREAMS: &[&str] = &["renode"];

impl Logs for EmulatorLogs {
    fn subscribe_structured(&self) -> broadcast::Receiver<LogEntry> {
        self.structured_tx.subscribe()
    }

    fn subscribe_hypervisor(&self) -> broadcast::Receiver<HypervisorLine> {
        self.hypervisor_tx.subscribe()
    }

    fn auxiliary_streams(&self) -> &[&str] {
        AUX_STREAMS
    }

    fn subscribe_auxiliary(&self, name: &str) -> Option<broadcast::Receiver<String>> {
        match name {
            "renode" => Some(self.renode_tx.subscribe()),
            _ => None,
        }
    }
}

/// Drain the emulator's mpsc channel and forward to broadcast channels.
fn bridge_logs(
    rx: mpsc::Receiver<UsartLog>,
    structured_tx: broadcast::Sender<LogEntry>,
    hypervisor_tx: broadcast::Sender<HypervisorLine>,
    renode_tx: broadcast::Sender<String>,
) {
    while let Ok(log) = rx.recv() {
        match log.kind {
            UsartLogKind::Line(text) => {
                let _ = hypervisor_tx.send(HypervisorLine { text });
            }
            UsartLogKind::Renode(text) => {
                if renode_tx.send(text.clone()).is_err() {
                    // No subscribers yet (startup) — print directly.
                    eprintln!("[renode] {text}");
                }
            }
            UsartLogKind::Stream(stream) => {
                // Collect all values from the stream's sub-channel.
                let mut values = Vec::new();
                while let Ok(v) = stream.values.recv() {
                    values.push(v);
                }

                let _ = structured_tx.send(LogEntry {
                    level: stream.metadata.level,
                    timestamp: stream.metadata.timestamp,
                    source: stream.metadata.source,
                    log_id: stream.metadata.log_id,
                    log_species: stream.metadata.log_species,
                    values,
                });
            }
        }
    }
}
