//! Persistent registry of configured serial ports.
//!
//! Stored at `~/.rcard/configured_ports.json`. Keyed by stable USB
//! identifiers (vid, pid, serial number, interface) so the same physical
//! adapter is recognized across reboots even if its OS COM/tty name
//! changes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};

use crate::state::SerialAdapterType;

/// Stable identifier for a USB serial adapter, derived from `UsbPortInfo`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PortIdentity {
    pub vid: u16,
    pub pid: u16,
    pub serial_number: Option<String>,
    pub interface: Option<u8>,
}

impl PortIdentity {
    pub fn from_usb(info: &UsbPortInfo) -> Self {
        Self {
            vid: info.vid,
            pid: info.pid,
            serial_number: info.serial_number.clone(),
            interface: info.interface,
        }
    }

    /// Human-readable label for the adapter (manufacturer + product if
    /// available, falling back to vid:pid).
    pub fn label(info: &UsbPortInfo) -> String {
        let parts: Vec<&str> = [info.manufacturer.as_deref(), info.product.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        if parts.is_empty() {
            format!("{:04x}:{:04x}", info.vid, info.pid)
        } else {
            parts.join(" ")
        }
    }
}

/// Per-port settings the user has chosen.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortConfiguration {
    pub adapter_type: SerialAdapterType,
}

#[derive(Default, Serialize, Deserialize)]
pub struct PortRegistry {
    /// We use a Vec<(K, V)> rather than HashMap because `PortIdentity`
    /// holds Option<String> fields and serializing as a JSON map would
    /// require keys to be strings. Vec keeps the wire format clean.
    entries: Vec<(PortIdentity, PortConfiguration)>,
}

impl PortRegistry {
    /// Path to `~/.rcard/configured_ports.json`. Returns None if the
    /// home directory can't be determined.
    pub fn path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".rcard").join("configured_ports.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                eprintln!("[port_registry] failed to parse {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(s) => {
                if let Err(e) = std::fs::write(&path, s) {
                    eprintln!("[port_registry] failed to write {}: {e}", path.display());
                }
            }
            Err(e) => eprintln!("[port_registry] failed to serialize: {e}"),
        }
    }

    pub fn get(&self, identity: &PortIdentity) -> Option<&PortConfiguration> {
        self.entries
            .iter()
            .find(|(k, _)| k == identity)
            .map(|(_, v)| v)
    }

    pub fn insert(&mut self, identity: PortIdentity, config: PortConfiguration) {
        if let Some((_, v)) = self.entries.iter_mut().find(|(k, _)| *k == identity) {
            *v = config;
        } else {
            self.entries.push((identity, config));
        }
    }

    pub fn remove(&mut self, identity: &PortIdentity) {
        self.entries.retain(|(k, _)| k != identity);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PortIdentity, &PortConfiguration)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }
}

/// A serial port currently visible to the OS, with the metadata we use
/// to identify it.
#[derive(Clone, Debug)]
pub struct AvailablePort {
    /// OS-level name (`COM16`, `/dev/ttyUSB0`, etc.).
    pub port_name: String,
    pub identity: PortIdentity,
    pub label: String,
}

/// Enumerate all USB serial ports visible to the OS. Non-USB ports are
/// skipped — we can't identify them stably.
pub fn available_usb_ports() -> Vec<AvailablePort> {
    let Ok(ports) = serialport::available_ports() else {
        return Vec::new();
    };
    ports
        .into_iter()
        .filter_map(|SerialPortInfo { port_name, port_type }| match port_type {
            SerialPortType::UsbPort(info) => Some(AvailablePort {
                port_name,
                identity: PortIdentity::from_usb(&info),
                label: PortIdentity::label(&info),
            }),
            _ => None,
        })
        .collect()
}
