use std::any::{Any, TypeId};
use std::io::{Cursor, Read as _};
use std::path::Path;
use std::sync::{mpsc, Arc};

use device::adapter::{Adapter, AdapterId};
use device::capability::CapabilitySet;
use device::device::{Device, DeviceEvent, LogSink};
use device::logs::LogEntry;
use tokio::sync::broadcast;
use zip::ZipArchive;

use crate::peripherals::usart::log::{UsartLog, UsartLogKind};
use crate::{DeviceBuilder, EmulatorError};

/// An emulated device running in Renode.
///
/// All-or-nothing lifecycle: `start()` boots the emulator, drop kills it.
pub struct EmulatedDevice {
    events_tx: broadcast::Sender<DeviceEvent>,
    capabilities: CapabilitySet,
    _run_thread: std::thread::JoinHandle<()>,
}

impl EmulatedDevice {
    /// Boot the emulator from a `.tfw` archive.
    ///
    /// Extracts `places.bin` and `renode_platform.repl` from the archive,
    /// then starts Renode and runs the firmware.
    pub fn start(tfw: &Path) -> Result<Self, EmulatorError> {
        let tfw_bytes = std::fs::read(tfw).map_err(EmulatorError::TempFile)?;
        let mut archive = ZipArchive::new(Cursor::new(tfw_bytes))
            .map_err(|e| EmulatorError::InvalidPlaces(format!("invalid tfw archive: {e}")))?;

        let places_bin = read_entry(&mut archive, "places.bin")?;
        let platform_repl = String::from_utf8(read_entry(&mut archive, "renode_platform.repl")?)
            .map_err(|e| {
                EmulatorError::InvalidPlaces(format!("renode_platform.repl is not valid UTF-8: {e}"))
            })?;

        let (events_tx, _) = broadcast::channel(256);

        // Create LogSinks for virtual adapters.
        let usart1_sink = LogSink::new(AdapterId(0), events_tx.clone());
        let usart2_sink = LogSink::new(AdapterId(1), events_tx.clone());
        let renode_sink = LogSink::new(AdapterId(2), events_tx.clone());
        let error_sink = LogSink::new(AdapterId(0), events_tx.clone());

        let run_thread = std::thread::spawn(move || {
            let (log_tx, log_rx) = mpsc::channel();

            std::thread::spawn(move || {
                bridge_logs(log_rx, usart1_sink, usart2_sink, renode_sink);
            });

            let mut device = match DeviceBuilder::new()
                .with_logger(log_tx)
                .with_platform(platform_repl)
                .with_places(places_bin)
                .build()
            {
                Ok(d) => d,
                Err(e) => {
                    error_sink.error(e);
                    return;
                }
            };

            if let Err(e) = device.run() {
                error_sink.error(e);
            }
        });

        Ok(EmulatedDevice {
            events_tx,
            capabilities: CapabilitySet::new(),
            _run_thread: run_thread,
        })
    }
}

fn read_entry(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    name: &str,
) -> Result<Vec<u8>, EmulatorError> {
    let mut entry = archive
        .by_name(name)
        .map_err(|e| EmulatorError::InvalidPlaces(format!("missing {name} in tfw: {e}")))?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry
        .read_to_end(&mut buf)
        .map_err(EmulatorError::TempFile)?;
    Ok(buf)
}

impl Device for EmulatedDevice {
    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.events_tx.subscribe()
    }

    fn query_capability(&self, type_id: TypeId) -> Option<Arc<dyn Any + Send + Sync>> {
        self.capabilities.query(type_id)
    }

    fn query_all_capabilities(
        &self,
        type_id: TypeId,
    ) -> Vec<(AdapterId, Arc<dyn Any + Send + Sync>)> {
        self.capabilities.query_all(type_id)
    }

    fn has_capability(&self, type_id: TypeId) -> bool {
        self.capabilities.has(type_id)
    }

    fn adapters(&self) -> Vec<(AdapterId, &dyn Adapter)> {
        // Emulated adapters are virtual — no Adapter trait objects to return.
        vec![]
    }
}

/// Drain the emulator's mpsc channel and forward to device via LogSinks.
///
/// Streams (USART2) are handled on separate threads so they don't block
/// USART1/Renode log delivery while waiting for values.
fn bridge_logs(
    rx: mpsc::Receiver<UsartLog>,
    usart1_sink: LogSink,
    usart2_sink: LogSink,
    renode_sink: LogSink,
) {
    while let Ok(log) = rx.recv() {
        match log.kind {
            UsartLogKind::Line(text) => {
                usart1_sink.text(text);
            }
            UsartLogKind::Renode(text) => {
                parse_renode_log(&renode_sink, text);
            }
            UsartLogKind::Stream(stream) => {
                usart2_sink.structured(LogEntry {
                    level: stream.metadata.level,
                    timestamp: stream.metadata.timestamp,
                    source: stream.metadata.source,
                    log_id: stream.metadata.log_id,
                    log_species: stream.metadata.log_species,
                    values: stream.values,
                    truncated: stream.truncated,
                });
            }
        }
    }
}

/// Parse Renode's `HH:MM:SS.FFFF [LEVEL] message` format.
fn parse_renode_log(sink: &LogSink, text: String) {
    let trimmed = text.trim();

    // Try to find "[LEVEL] " pattern.
    let parsed = trimmed.find('[').and_then(|bracket| {
        let end = trimmed[bracket..].find(']')?;
        let level_str = &trimmed[bracket + 1..bracket + end];
        let message_start = bracket + end + 1;
        let message = trimmed[message_start..].trim_start();
        if message.is_empty() {
            return None;
        }

        let level = match level_str {
            "ERROR" | "FATAL" => rcard_log::LogLevel::Error,
            "WARNING" => rcard_log::LogLevel::Warn,
            "INFO" => rcard_log::LogLevel::Info,
            "DEBUG" | "NOISY" => rcard_log::LogLevel::Debug,
            "TRACE" => rcard_log::LogLevel::Trace,
            _ => return None,
        };

        Some((level, message.to_string()))
    });

    match parsed {
        Some((level, message)) => sink.renode(level, message),
        None => sink.auxiliary("renode".into(), text),
    }
}
