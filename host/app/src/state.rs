use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use device::adapter::AdapterId;
use device::logs::Log;
use tfw::archive::TfwMetadata;

use crate::panels::Pane;

/// Stable device identifier. Monotonic, eventually persisted in config.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DeviceId(pub u64);

/// Stable firmware identifier. Monotonic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FirmwareId(pub u64);

/// Stable build identifier. Monotonic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BuildId(pub u64);

/// 128-bit chip UID from eFuse bank 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChipUid(pub [u8; 16]);

impl std::fmt::Display for ChipUid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

// ── Activity bar / sidebar ──────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarSection {
    Firmware,
    Adapters,
    Devices,
}

// ── Serial port config ─────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SerialAdapterType {
    Usart1,
    Usart2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerialPortStatus {
    /// Background loop is trying to open the port.
    Connecting,
    /// Port is open, waiting for valid device data.
    PortOpen,
    /// Device detected — ephemeral device created.
    DeviceDetected,
    /// Port couldn't be opened (retrying).
    Error,
}

/// A configured serial port in the adapters sidebar.
pub struct SerialPortConfig {
    pub port: String,
    pub adapter_type: SerialAdapterType,
    pub status: SerialPortStatus,
    /// USB identity for the persistent registry. Always Some for ports
    /// added via the dropdown; reserved as Option for future expansion.
    pub identity: Option<crate::port_registry::PortIdentity>,
    /// Device ID created when device is detected (None otherwise).
    pub device_id: Option<DeviceId>,
    /// USART1 only: raw text lines off the wire. Unbounded scrollback.
    pub raw_lines: std::collections::VecDeque<String>,
    /// USART2 only: decoded structured log entries. Unbounded scrollback.
    pub structured_logs: Vec<device::logs::LogEntry>,
    /// USART2 only: decoded IPC control events (tunnel errors, replies,
    /// frame-decode errors). Unbounded scrollback.
    pub control_events: Vec<device::logs::ControlEvent>,
}

/// Stable serial config index (not an ID — just Vec position).
pub type SerialPortIndex = usize;

// ── Firmware ────────────────────────────────────────────────────────────

pub struct FirmwareHandle {
    pub id: FirmwareId,
    /// On-disk location, or `None` for builtin (in-memory) entries like the
    /// embedded stub firmware.
    pub path: Option<PathBuf>,
    pub metadata: TfwMetadata,
}

impl FirmwareHandle {
    pub fn is_builtin(&self) -> bool {
        self.path.is_none()
    }

    /// Short display name derived from the build metadata or filename.
    pub fn display_name(&self) -> String {
        if let Some(build) = &self.metadata.build {
            let ver = build.version.as_deref().unwrap_or("?");
            let short_id = &build.build_id[..8.min(build.build_id.len())];
            let tag = if self.is_builtin() { " (builtin)" } else { "" };
            format!("{} v{ver}{tag} - {short_id}", build.name)
        } else if let Some(path) = &self.path {
            path.file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".into())
        } else {
            "builtin".into()
        }
    }

    /// Build ID if available.
    pub fn build_id(&self) -> Option<&str> {
        self.metadata.build.as_ref().map(|b| b.build_id.as_str())
    }
}

// ── Capabilities (GUI-side tracking) ────────────────────────────────────

/// Known capabilities that the GUI tracks for display and decision-making.
/// These mirror what the real capability system provides, but are just
/// presence flags — the actual capability objects live on the bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnownCapability {
    SifliDebug,
    Ipc,
    UsbIpc,
}

// ── Device ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Persistent,
    Ephemeral,
    Emulator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevicePhase {
    /// Power unstable — seeing repeated SFBL resets.
    Stabilizing,
    /// SiFli bootrom — stable, ready for debug entry.
    Bootrom,
    /// rcard bootloader running — saw "bootloader: Awake\r\n".
    Bootloader,
    /// Hubris kernel running — saw "kernel: Awake\r\n".
    Kernel,
    /// Phase unknown (e.g. emulator, or no sentinel seen yet).
    Unknown,
}

pub struct DeviceHandle {
    pub id: DeviceId,
    pub name: String,
    pub kind: DeviceKind,
    pub phase: DevicePhase,
    /// Chip UID if identified via SifliDebug.
    pub uid: Option<ChipUid>,
    /// Known capabilities on this device.
    pub capabilities: HashSet<KnownCapability>,
    /// Adapter IDs associated with this device.
    pub adapter_ids: Vec<AdapterId>,
    /// The firmware this device is running, if known.
    pub firmware_id: Option<FirmwareId>,
    /// Sorted by `received_at` via binary-insertion in [`push_log`].
    /// Logs from multiple adapters (e.g. USART1 text + USART2 structured)
    /// land in correct device-emission order because each adapter stamps
    /// its logs at first-byte-receipt before they reach the main thread.
    pub log_buffer: Vec<Log>,
    /// IPC schema registry — loaded from the matched firmware's tfw
    /// metadata when `DeviceReportedBuildId` fires. `None` if no tfw
    /// matched or if the tfw had no schema data.
    pub ipc_registry: Option<std::sync::Arc<ipc_runtime::Registry>>,
}

impl DeviceHandle {
    pub fn new(
        id: DeviceId,
        name: String,
        kind: DeviceKind,
        adapter_ids: Vec<AdapterId>,
        firmware_id: Option<FirmwareId>,
    ) -> Self {
        DeviceHandle {
            id,
            name,
            kind,
            phase: DevicePhase::Unknown,
            uid: None,
            capabilities: HashSet::new(),
            adapter_ids,
            firmware_id,
            log_buffer: Vec::new(),
            ipc_registry: None,
        }
    }

    pub fn push_log(&mut self, log: Log) {
        // Sorted insertion by `received_at`. `partition_point` finds the
        // first index whose stamp is *strictly greater* than the new
        // log's stamp; inserting there keeps ties in arrival order
        // (stable) — which is what we want for logs sharing a wall-clock
        // nanosecond.
        let idx = self
            .log_buffer
            .partition_point(|existing| existing.received_at <= log.received_at);
        self.log_buffer.insert(idx, log);
    }

    pub fn is_connected(&self) -> bool {
        !self.adapter_ids.is_empty()
    }
}

/// A frontend-tracked adapter.
pub struct AdapterHandle {
    pub id: AdapterId,
    pub display_name: String,
}

impl AdapterHandle {
    pub fn new(id: AdapterId, display_name: String) -> Self {
        AdapterHandle { id, display_name }
    }
}

// ── Build state ────────────────────────────────────────────────────────

/// Configuration for a firmware build.
#[derive(Clone, Debug)]
pub struct BuildConfig {
    pub config: String,
    pub board: String,
    pub layout: String,
}

/// Current status of a build.
pub enum BuildStatus {
    Running {
        stage: String,
        detail: String,
    },
    Succeeded {
        tfw_path: PathBuf,
        firmware_id: Option<FirmwareId>,
    },
    Failed {
        error: String,
    },
}

/// A build tracked by the app.
pub struct BuildHandle {
    pub id: BuildId,
    pub config: BuildConfig,
    pub status: BuildStatus,
    pub log: Vec<String>,
}

// ── File drag state ─────────────────────────────────────────────────────

/// Tracks a pane being dragged into the tree from an external file hover.
pub struct FileDragState {
    /// The firmware ID that was loaded for this drag.
    pub firmware_id: FirmwareId,
    /// The tile ID inserted into the tree (so we can clean up on cancel).
    pub tile_id: egui_tiles::TileId,
    /// Set to true on the drop frame so cleanup knows it was a real drop.
    pub dropped: bool,
}

// ── Flash modal state ───────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashMethod {
    /// Device has USB — flash stub via existing firmware's USB, then flash target via stub.
    Usb,
    /// Device has USART1 only — flash stub via SifliDebug, then flash target via stub USB.
    SifliDebug,
}

pub enum FlashModalState {
    /// Picking a device to flash.
    Picker {
        firmware_id: FirmwareId,
        selected_device: Option<DeviceId>,
    },
    /// Flash in progress — modal shows live progress.
    Flashing {
        firmware_id: FirmwareId,
        device_id: DeviceId,
        phase: crate::bridge::FlashPhase,
    },
}

// ── Sidebar panes (for egui_tiles split) ────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarPane {
    Build,
    FirmwareList,
}

// ── Top-level state ─────────────────────────────────────────────────────

pub struct AppState {
    // Sidebar.
    pub sidebar_section: SidebarSection,
    pub firmware_sidebar_tree: egui_tiles::Tree<SidebarPane>,

    // Main area — tiled panels via egui_tiles.
    pub tree: egui_tiles::Tree<Pane>,

    // Data.
    pub adapters: HashMap<AdapterId, AdapterHandle>,
    pub devices: HashMap<DeviceId, DeviceHandle>,
    pub firmware: HashMap<FirmwareId, FirmwareHandle>,
    pub builds: HashMap<BuildId, BuildHandle>,
    next_device_id: u64,
    next_firmware_id: u64,
    next_build_id: u64,

    // Flash modal.
    pub flash_modal: Option<FlashModalState>,

    // Serial adapter configs.
    pub serial_ports: Vec<SerialPortConfig>,
    /// Persistent on-disk record of configured ports, keyed by USB identity.
    pub port_registry: crate::port_registry::PortRegistry,
    /// Cached snapshot of OS-visible USB serial ports, refreshed on demand.
    pub available_ports: Vec<crate::port_registry::AvailablePort>,
    /// Index into `available_ports` selected in the "Add port" form.
    pub new_port_selection: Option<usize>,
    pub new_port_type: SerialAdapterType,

    // Build configuration (sidebar state).
    pub firmware_dir: PathBuf,
    pub firmware_dir_input: String,
    pub build_configs: Vec<String>,
    pub build_boards: Vec<String>,
    pub build_layouts: Vec<String>,
    pub selected_config: usize,
    pub selected_board: usize,
    pub selected_layout: usize,

    // File drop drag state — tracks a pane being dragged from an external file hover.
    pub file_drag: Option<FileDragState>,

    // Channels.
    pub cmd_tx: tokio::sync::mpsc::UnboundedSender<crate::bridge::Command>,
    pub event_rx: crossbeam_channel::Receiver<crate::bridge::Event>,
}

impl AppState {
    pub fn new(
        cmd_tx: tokio::sync::mpsc::UnboundedSender<crate::bridge::Command>,
        event_rx: crossbeam_channel::Receiver<crate::bridge::Event>,
    ) -> Self {
        let firmware_dir = PathBuf::new();
        let firmware_dir_input = String::new();

        let build_configs: Vec<String> = Vec::new();
        let build_boards = Vec::new();
        let build_layouts = Vec::new();

        let firmware_sidebar_tree = {
            let mut tiles = egui_tiles::Tiles::default();
            let build = tiles.insert_pane(SidebarPane::Build);
            let fw_list = tiles.insert_pane(SidebarPane::FirmwareList);
            let root = tiles.insert_vertical_tile(vec![build, fw_list]);
            egui_tiles::Tree::new("fw_sidebar", root, tiles)
        };

        let port_registry = crate::port_registry::PortRegistry::load();
        let available_ports = crate::port_registry::available_usb_ports();

        let mut state = AppState {
            sidebar_section: SidebarSection::Devices,
            firmware_sidebar_tree,
            tree: egui_tiles::Tree::empty("main_tree"),
            adapters: HashMap::new(),
            devices: HashMap::new(),
            firmware: HashMap::new(),
            builds: HashMap::new(),
            next_device_id: 0,
            next_firmware_id: 0,
            next_build_id: 0,
            flash_modal: None,
            serial_ports: Vec::new(),
            port_registry,
            available_ports,
            new_port_selection: None,
            new_port_type: SerialAdapterType::Usart1,
            firmware_dir,
            firmware_dir_input,
            build_configs,
            build_boards,
            build_layouts,
            selected_config: 0,
            selected_board: 0,
            selected_layout: 0,
            file_drag: None,
            cmd_tx,
            event_rx,
        };

        // Auto-register every port from the registry that we can see right
        // now. Ports that aren't currently plugged in are simply skipped;
        // they stay in the registry and will be picked up on next startup
        // if they reappear.
        let plan: Vec<(crate::port_registry::AvailablePort, SerialAdapterType)> = state
            .port_registry
            .iter()
            .filter_map(|(identity, cfg)| {
                state
                    .available_ports
                    .iter()
                    .find(|p| p.identity == *identity)
                    .map(|p| (p.clone(), cfg.adapter_type))
            })
            .collect();
        for (port, adapter_type) in plan {
            state.start_serial_port(port, adapter_type, false);
        }

        state
    }

    /// Re-scan the OS for USB serial ports and update `available_ports`.
    pub fn refresh_available_ports(&mut self) {
        self.available_ports = crate::port_registry::available_usb_ports();
    }

    /// `available_ports` minus any port already in `serial_ports`.
    pub fn unconfigured_available_ports(&self) -> Vec<&crate::port_registry::AvailablePort> {
        self.available_ports
            .iter()
            .filter(|p| {
                !self
                    .serial_ports
                    .iter()
                    .any(|cfg| cfg.identity.as_ref() == Some(&p.identity))
            })
            .collect()
    }

    /// Start a serial port: create the SerialPortConfig, optionally
    /// persist to the registry, send RegisterSerial to the bridge.
    fn start_serial_port(
        &mut self,
        port: crate::port_registry::AvailablePort,
        adapter_type: SerialAdapterType,
        persist: bool,
    ) {
        if persist {
            self.port_registry.insert(
                port.identity.clone(),
                crate::port_registry::PortConfiguration { adapter_type },
            );
            self.port_registry.save();
        }

        let index = self.serial_ports.len();
        self.serial_ports.push(SerialPortConfig {
            port: port.port_name.clone(),
            adapter_type,
            status: SerialPortStatus::Connecting,
            identity: Some(port.identity),
            device_id: None,
            raw_lines: std::collections::VecDeque::new(),
            structured_logs: Vec::new(),
            control_events: Vec::new(),
        });

        let _ = self.cmd_tx.send(crate::bridge::Command::RegisterSerial {
            index,
            port: port.port_name,
            adapter_type,
        });
    }

    pub fn next_device_id(&mut self) -> DeviceId {
        let id = DeviceId(self.next_device_id);
        self.next_device_id += 1;
        id
    }

    pub fn next_firmware_id(&mut self) -> FirmwareId {
        let id = FirmwareId(self.next_firmware_id);
        self.next_firmware_id += 1;
        id
    }

    pub fn next_build_id(&mut self) -> BuildId {
        let id = BuildId(self.next_build_id);
        self.next_build_id += 1;
        id
    }

    /// Permanently remove a device and clean up its tiles and serial config references.
    /// Only use for actual deletion — disconnection is handled by adapter removal.
    fn remove_device(&mut self, device_id: DeviceId) {
        self.devices.remove(&device_id);
        // Clear serial config references.
        for cfg in &mut self.serial_ports {
            if cfg.device_id == Some(device_id) {
                cfg.device_id = None;
                cfg.status = SerialPortStatus::Connecting;
            }
        }
        // Remove any open tiles for this device.
        let to_remove: Vec<egui_tiles::TileId> = self
            .tree
            .tiles
            .iter()
            .filter_map(|(tile_id, tile)| match tile {
                egui_tiles::Tile::Pane(Pane::DeviceLogs(d))
                | egui_tiles::Tile::Pane(Pane::DeviceProtocol(d))
                | egui_tiles::Tile::Pane(Pane::DeviceRenode(d))
                    if *d == device_id =>
                {
                    Some(*tile_id)
                }
                _ => None,
            })
            .collect();
        for tile_id in to_remove {
            self.tree.remove_recursively(tile_id);
        }
    }

    /// Disconnect a device (clear adapters, reset phase) without removing it.
    fn disconnect_device(&mut self, device_id: DeviceId) {
        if let Some(dev) = self.devices.get_mut(&device_id) {
            dev.adapter_ids.clear();
            dev.phase = DevicePhase::Unknown;
        }
    }

    /// Look up an adapter's display name.
    pub fn adapter_name(&self, id: AdapterId) -> &str {
        self.adapters
            .get(&id)
            .map(|a| a.display_name.as_str())
            .unwrap_or("?")
    }

    /// Determine the flash method for a device based on its capabilities.
    pub fn flash_method_for_device(&self, device_id: DeviceId) -> Option<FlashMethod> {
        let dev = self.devices.get(&device_id)?;
        if dev.capabilities.contains(&KnownCapability::UsbIpc)
            || dev.capabilities.contains(&KnownCapability::Ipc)
        {
            Some(FlashMethod::Usb)
        } else if dev.capabilities.contains(&KnownCapability::SifliDebug) {
            Some(FlashMethod::SifliDebug)
        } else {
            None
        }
    }

    /// Re-scan the firmware directory for available configs/boards/layouts.
    pub fn refresh_build_options(&mut self) {
        self.firmware_dir = PathBuf::from(&self.firmware_dir_input);
        self.build_configs = discover_ncl_names(&self.firmware_dir, "apps");
        self.build_boards = discover_ncl_names(&self.firmware_dir, "boards");
        self.build_layouts = discover_ncl_names(&self.firmware_dir, "layouts");
        self.selected_config = 0;
        self.selected_board = 0;
        self.selected_layout = 0;
    }

    /// Import a .tfw file into the firmware database.
    ///
    /// Copies the file to `~/.rcard/firmware/<build_id>.tfw` and adds it
    /// to the in-memory firmware list. If a firmware with the same build_id
    /// already exists, returns the existing FirmwareId (no-op).
    pub fn load_firmware(&mut self, path: PathBuf) -> Result<FirmwareId, String> {
        let metadata = tfw::archive::load_metadata(&path).map_err(|e| e.to_string())?;

        // Dedup on build_id.
        if let Some(build_id) = metadata.build.as_ref().map(|b| b.build_id.as_str()) {
            if let Some((existing_id, _)) = self.firmware.iter().find(|(_, fw)| {
                fw.build_id() == Some(build_id)
            }) {
                return Ok(*existing_id);
            }
        }

        // Copy to database.
        let db_path = firmware_db_path(&metadata)?;
        if db_path != path {
            std::fs::copy(&path, &db_path)
                .map_err(|e| format!("copy to firmware database: {e}"))?;
        }

        let id = self.next_firmware_id();
        self.firmware.insert(
            id,
            FirmwareHandle {
                id,
                path: Some(db_path),
                metadata,
            },
        );
        Ok(id)
    }

    /// Register the compile-time-embedded stub firmware as an in-memory
    /// (ephemeral) entry so it appears in firmware lists and is searchable
    /// by id alongside disk-loaded firmware.
    pub fn register_builtin_stub(&mut self) {
        let metadata = match tfw::archive::load_metadata_from_bytes(crate::stub::TFW) {
            Ok(m) => m,
            Err(_) => return,
        };
        let id = self.next_firmware_id();
        self.firmware.insert(
            id,
            FirmwareHandle {
                id,
                path: None,
                metadata,
            },
        );
    }

    /// Scan the firmware database directory and load all .tfw files.
    pub fn scan_firmware_db(&mut self) {
        let db_dir = firmware_db_dir();
        let Ok(entries) = std::fs::read_dir(&db_dir) else {
            return;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "tfw") {
                if let Ok(metadata) = tfw::archive::load_metadata(&path) {
                    // Skip if already loaded (dedup on build_id).
                    let dominated = metadata.build.as_ref().is_some_and(|b| {
                        self.firmware.values().any(|fw| fw.build_id() == Some(&b.build_id))
                    });
                    if dominated {
                        continue;
                    }

                    let id = self.next_firmware_id();
                    self.firmware.insert(
                        id,
                        FirmwareHandle { id, path: Some(path), metadata },
                    );
                }
            }
        }
    }

    /// Open a device in the tile tree (Logs + Protocol, plus Renode for emulators).
    pub fn open_device(&mut self, id: DeviceId) {
        let already_open = self.tree.tiles.iter().any(|(_, tile)| match tile {
            egui_tiles::Tile::Pane(Pane::DeviceLogs(d))
            | egui_tiles::Tile::Pane(Pane::DeviceProtocol(d))
            | egui_tiles::Tile::Pane(Pane::DeviceRenode(d)) => *d == id,
            _ => false,
        });

        if !already_open {
            let is_emulator = self
                .devices
                .get(&id)
                .map(|d| d.kind == DeviceKind::Emulator)
                .unwrap_or(false);

            let logs = self.tree.tiles.insert_pane(Pane::DeviceLogs(id));
            let proto = self.tree.tiles.insert_pane(Pane::DeviceProtocol(id));
            let mut tabs = vec![logs, proto];
            if is_emulator {
                tabs.push(self.tree.tiles.insert_pane(Pane::DeviceRenode(id)));
            }
            let tab_group = self.tree.tiles.insert_tab_tile(tabs);

            if let Some(root) = self.tree.root() {
                let new_root = self
                    .tree
                    .tiles
                    .insert_horizontal_tile(vec![root, tab_group]);
                self.tree.root = Some(new_root);
            } else {
                self.tree.root = Some(tab_group);
            }
        }

        let target = Pane::DeviceLogs(id);
        self.tree
            .make_active(|_, tile| matches!(tile, egui_tiles::Tile::Pane(p) if *p == target));
    }

    /// Open a single device pane in the tile tree.
    pub fn open_device_pane(&mut self, pane: Pane) {
        let already_open = self.tree.tiles.iter().any(|(_, tile)| {
            matches!(tile, egui_tiles::Tile::Pane(p) if *p == pane)
        });

        if !already_open {
            let tile = self.tree.tiles.insert_pane(pane.clone());

            if let Some(root) = self.tree.root() {
                if let Some(egui_tiles::Tile::Container(container)) =
                    self.tree.tiles.get_mut(root)
                {
                    container.add_child(tile);
                } else {
                    let new_root = self
                        .tree
                        .tiles
                        .insert_horizontal_tile(vec![root, tile]);
                    self.tree.root = Some(new_root);
                }
            } else {
                let tab = self.tree.tiles.insert_tab_tile(vec![tile]);
                self.tree.root = Some(tab);
            }
        }

        self.tree
            .make_active(|_, tile| matches!(tile, egui_tiles::Tile::Pane(p) if *p == pane));
    }

    /// Open a USART2 serial port in the tile tree as a Logs + Control tab group.
    ///
    /// Mirrors `open_device`: if any sub-pane for this port is already open,
    /// focuses the Logs tab; otherwise creates a new tab group with both.
    pub fn open_serial_port(&mut self, idx: SerialPortIndex) {
        let already_open = self.tree.tiles.iter().any(|(_, tile)| match tile {
            egui_tiles::Tile::Pane(Pane::SerialAdapterLogs(i))
            | egui_tiles::Tile::Pane(Pane::SerialAdapterControl(i)) => *i == idx,
            _ => false,
        });

        if !already_open {
            let logs = self.tree.tiles.insert_pane(Pane::SerialAdapterLogs(idx));
            let ctrl = self.tree.tiles.insert_pane(Pane::SerialAdapterControl(idx));
            let tab_group = self.tree.tiles.insert_tab_tile(vec![logs, ctrl]);

            if let Some(root) = self.tree.root() {
                let new_root = self
                    .tree
                    .tiles
                    .insert_horizontal_tile(vec![root, tab_group]);
                self.tree.root = Some(new_root);
            } else {
                self.tree.root = Some(tab_group);
            }
        }

        let target = Pane::SerialAdapterLogs(idx);
        self.tree
            .make_active(|_, tile| matches!(tile, egui_tiles::Tile::Pane(p) if *p == target));
    }

    /// Open a firmware status panel in the tile tree.
    pub fn open_firmware(&mut self, id: FirmwareId) {
        let target = Pane::FirmwareStatus(id);

        let already_open = self.tree.tiles.iter().any(|(_, tile)| {
            matches!(tile, egui_tiles::Tile::Pane(p) if *p == target)
        });

        if !already_open {
            let tile = self.tree.tiles.insert_pane(target.clone());

            if let Some(root) = self.tree.root() {
                if let Some(egui_tiles::Tile::Container(container)) = self.tree.tiles.get_mut(root) {
                    container.add_child(tile);
                } else {
                    let new_root = self
                        .tree
                        .tiles
                        .insert_horizontal_tile(vec![root, tile]);
                    self.tree.root = Some(new_root);
                }
            } else {
                let tab = self.tree.tiles.insert_tab_tile(vec![tile]);
                self.tree.root = Some(tab);
            }
        }

        self.tree
            .make_active(|_, tile| matches!(tile, egui_tiles::Tile::Pane(p) if *p == target));
    }

    pub fn drain_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                crate::bridge::Event::AdapterCreated {
                    adapter_id,
                    display_name,
                } => {
                    self.adapters.insert(
                        adapter_id,
                        AdapterHandle::new(adapter_id, display_name),
                    );
                }
                crate::bridge::Event::AdapterRemoved { adapter_id } => {
                    self.adapters.remove(&adapter_id);
                    // Remove this adapter from any devices that reference it.
                    for dev in self.devices.values_mut() {
                        dev.adapter_ids.retain(|id| *id != adapter_id);
                    }
                    // Cleanup: delete ephemeral and emulator devices with no adapters.
                    // Only persistent devices survive disconnection.
                    let to_remove: Vec<DeviceId> = self
                        .devices
                        .iter()
                        .filter(|(_, dev)| {
                            matches!(dev.kind, DeviceKind::Ephemeral | DeviceKind::Emulator)
                                && dev.adapter_ids.is_empty()
                        })
                        .map(|(id, _)| *id)
                        .collect();
                    for device_id in to_remove {
                        self.remove_device(device_id);
                    }
                    // Persistent devices with no adapters → disconnected.
                    for dev in self.devices.values_mut() {
                        if dev.kind == DeviceKind::Persistent && dev.adapter_ids.is_empty() {
                            dev.phase = DevicePhase::Unknown;
                        }
                    }
                }
                crate::bridge::Event::DeviceCreated {
                    device_id,
                    name,
                    kind,
                    adapter_ids,
                    capabilities,
                    firmware_id,
                } => {
                    // Idempotent: two adapters on the same chip (e.g. USART1
                    // and USART2) both fire `DeviceCreated` for the same
                    // persistent `DeviceId` resolved through the shared
                    // `persistent_devices` registry. Treat the second call
                    // as "attach this adapter + merge capabilities" rather
                    // than replacing the whole handle — otherwise the
                    // existing `log_buffer` and peer adapter bindings get
                    // wiped.
                    let is_emulator;
                    if let Some(existing) = self.devices.get_mut(&device_id) {
                        is_emulator = existing.kind == DeviceKind::Emulator;
                        for id in adapter_ids {
                            if !existing.adapter_ids.contains(&id) {
                                existing.adapter_ids.push(id);
                            }
                        }
                        existing.capabilities.extend(capabilities);
                        // Upgrade ephemeral → persistent if the new binding
                        // carries a stronger kind. Persistent > Ephemeral;
                        // don't downgrade.
                        if existing.kind == DeviceKind::Ephemeral
                            && kind == DeviceKind::Persistent
                        {
                            existing.kind = DeviceKind::Persistent;
                            existing.name = name;
                        }
                        if existing.firmware_id.is_none() {
                            existing.firmware_id = firmware_id;
                        }
                    } else {
                        let mut dev = DeviceHandle::new(
                            device_id, name, kind, adapter_ids, firmware_id,
                        );
                        dev.capabilities = capabilities.into_iter().collect();
                        is_emulator = dev.kind == DeviceKind::Emulator;
                        self.devices.insert(device_id, dev);
                    }

                    // Update serial config if this came from a serial connection.
                    for cfg in &mut self.serial_ports {
                        if cfg.device_id.is_none()
                            && cfg.status == SerialPortStatus::DeviceDetected
                        {
                            cfg.device_id = Some(device_id);
                            break;
                        }
                    }
                    // Auto-open emulator devices on creation.
                    if is_emulator {
                        self.open_device(device_id);
                    }
                }
                crate::bridge::Event::DeviceDeleted { device_id } => {
                    self.remove_device(device_id);
                }
                crate::bridge::Event::Log { device, log } => {
                    if let Some(dev) = self.devices.get_mut(&device) {
                        dev.push_log(log);
                    }
                }
                crate::bridge::Event::BuildStage {
                    build_id,
                    stage,
                    detail,
                } => {
                    if let Some(build) = self.builds.get_mut(&build_id) {
                        build.log.push(format!("[{stage}] {detail}"));
                        build.status = BuildStatus::Running { stage, detail };
                    }
                }
                crate::bridge::Event::BuildLog { build_id, message } => {
                    if let Some(build) = self.builds.get_mut(&build_id) {
                        build.log.push(message);
                    }
                }
                crate::bridge::Event::BuildComplete { build_id, result } => {
                    match result {
                        Ok(tfw_path) => {
                            let fw_id = self.load_firmware(tfw_path.clone()).ok();
                            if let Some(build) = self.builds.get_mut(&build_id) {
                                build.status = BuildStatus::Succeeded {
                                    tfw_path,
                                    firmware_id: fw_id,
                                };
                            }
                            if let Some(fw_id) = fw_id {
                                self.open_firmware(fw_id);
                            }
                        }
                        Err(error) => {
                            if let Some(build) = self.builds.get_mut(&build_id) {
                                build.log.push(format!("ERROR: {error}"));
                                build.status = BuildStatus::Failed { error };
                            }
                        }
                    }
                }
                crate::bridge::Event::DevicePhaseChanged { device_id, phase } => {
                    if let Some(dev) = self.devices.get_mut(&device_id) {
                        dev.phase = phase;
                    }
                }
                crate::bridge::Event::DeviceUpgraded { old_id, new_id } => {
                    // Migrate logs and phase from old (ephemeral) to new (persistent).
                    if let Some(old_dev) = self.devices.remove(&old_id) {
                        if let Some(new_dev) = self.devices.get_mut(&new_id) {
                            // Migrate via `push_log` so sort order is
                            // preserved even if the target device already
                            // has logs from another adapter.
                            for log in old_dev.log_buffer {
                                new_dev.push_log(log);
                            }
                            new_dev.phase = old_dev.phase;
                        }
                    }

                    // Migrate any open tiles from old → new.
                    let tile_updates: Vec<(egui_tiles::TileId, Pane)> = self.tree.tiles
                        .iter()
                        .filter_map(|(tile_id, tile)| match tile {
                            egui_tiles::Tile::Pane(Pane::DeviceLogs(d)) if *d == old_id => {
                                Some((*tile_id, Pane::DeviceLogs(new_id)))
                            }
                            egui_tiles::Tile::Pane(Pane::DeviceProtocol(d)) if *d == old_id => {
                                Some((*tile_id, Pane::DeviceProtocol(new_id)))
                            }
                            _ => None,
                        })
                        .collect();
                    for (tile_id, new_pane) in tile_updates {
                        if let Some(egui_tiles::Tile::Pane(p)) = self.tree.tiles.get_mut(tile_id) {
                            *p = new_pane;
                        }
                    }

                    // Update serial config references.
                    for cfg in &mut self.serial_ports {
                        if cfg.device_id == Some(old_id) {
                            cfg.device_id = Some(new_id);
                        }
                    }

                    // Update flash modal if tracking the old device.
                    match &mut self.flash_modal {
                        Some(FlashModalState::Picker { selected_device, .. })
                            if *selected_device == Some(old_id) =>
                        {
                            *selected_device = Some(new_id);
                        }
                        Some(FlashModalState::Flashing { device_id, .. })
                            if *device_id == old_id =>
                        {
                            *device_id = new_id;
                        }
                        _ => {}
                    }
                }
                crate::bridge::Event::FlashProgress { device_id, phase } => {
                    // Smoke test: when the stub is up (Done), try an IPC
                    // call to Log::log to verify the end-to-end path.
                    if matches!(&phase, crate::bridge::FlashPhase::Done) {
                        eprintln!("[flash] stub up — testing IPC call to Log::log");
                        let test_args = ipc_runtime::IpcValue::Struct(indexmap::indexmap! {
                            "level".into() => ipc_runtime::IpcValue::Enum {
                                variant: "Info".into(),
                                payload: vec![],
                            },
                            "species".into() => ipc_runtime::IpcValue::U64(0),
                            "argument_stream".into() => ipc_runtime::IpcValue::Bytes(vec![]),
                        });
                        match self.ipc_call(device_id, "Log", "log", test_args) {
                            Ok(rx) => {
                                eprintln!("[flash] IPC call sent, awaiting reply...");
                                std::thread::spawn(move || {
                                    match rx.blocking_recv() {
                                        Ok(Ok(result)) => eprintln!(
                                            "[flash] IPC reply: rc={} return={} bytes",
                                            result.rc,
                                            result.return_value.len(),
                                        ),
                                        Ok(Err(e)) => eprintln!("[flash] IPC transport error: {e}"),
                                        Err(_) => eprintln!("[flash] IPC reply channel dropped (timeout?)"),
                                    }
                                });
                            }
                            Err(e) => eprintln!("[flash] IPC call failed: {e}"),
                        }
                    }

                    match &self.flash_modal {
                        Some(FlashModalState::Picker { firmware_id, selected_device })
                            if *selected_device == Some(device_id) =>
                        {
                            let firmware_id = *firmware_id;
                            self.flash_modal = Some(FlashModalState::Flashing {
                                firmware_id,
                                device_id,
                                phase,
                            });
                        }
                        Some(FlashModalState::Flashing { device_id: d, .. })
                            if *d == device_id =>
                        {
                            if let Some(FlashModalState::Flashing { phase: p, .. }) =
                                &mut self.flash_modal
                            {
                                *p = phase;
                            }
                        }
                        _ => {}
                    }
                }
                crate::bridge::Event::SerialStatus { index, status } => {
                    if let Some(cfg) = self.serial_ports.get_mut(index) {
                        cfg.status = status;
                    }
                }
                crate::bridge::Event::SerialRawLine { index, line } => {
                    if let Some(cfg) = self.serial_ports.get_mut(index) {
                        cfg.raw_lines.push_back(line);
                    }
                }
                crate::bridge::Event::SerialStructuredLog { index, entry } => {
                    if let Some(cfg) = self.serial_ports.get_mut(index) {
                        cfg.structured_logs.push(entry);
                    }
                }
                crate::bridge::Event::SerialControlEvent { index, event } => {
                    if let Some(cfg) = self.serial_ports.get_mut(index) {
                        cfg.control_events.push(event);
                    }
                }
                crate::bridge::Event::DeviceReportedBuildId {
                    device_id,
                    build_id_bytes,
                } => {
                    // Find the loaded .tfw archive whose build_id
                    // (UUID string) parses to the same 16 bytes the
                    // device just reported. On a match, bind it to the
                    // device so the log viewer can resolve species and
                    // type metadata.
                    let matched_fw_id = self.firmware.iter().find_map(|(fw_id, fw)| {
                        let build_id_str = fw.build_id()?;
                        let parsed = uuid::Uuid::parse_str(build_id_str).ok()?;
                        if *parsed.as_bytes() == build_id_bytes {
                            Some(*fw_id)
                        } else {
                            None
                        }
                    });
                    if let Some(fw_id) = matched_fw_id {
                        if let Some(dev) = self.devices.get_mut(&device_id) {
                            dev.firmware_id = Some(fw_id);

                            // Build IPC registry from the firmware's schema metadata.
                            if let Some(fw) = self.firmware.get(&fw_id) {
                                if fw.metadata.ipc.is_none() {
                                    eprintln!("[ipc] firmware matched but tfw has no ipc metadata");
                                } else if fw.metadata.ipc.as_ref().unwrap().schemas.is_none() {
                                    eprintln!(
                                        "[ipc] firmware matched but tfw has no schema data \
                                         (rebuild firmware to populate ipc.schemas)"
                                    );
                                }
                                if let Some(ref ipc) = fw.metadata.ipc {
                                    if let Some(ref schemas) = ipc.schemas {
                                        // Serialize server metadata to JSON for the registry
                                        // to resolve task_ids.
                                        let servers_json =
                                            serde_json::to_value(&ipc.servers).ok();
                                        match ipc_runtime::Registry::from_schemas_json(
                                            schemas,
                                            servers_json.as_ref(),
                                        ) {
                                            Ok(registry) => {
                                                dev.ipc_registry =
                                                    Some(std::sync::Arc::new(registry));
                                            }
                                            Err(e) => {
                                                eprintln!(
                                                    "warning: failed to build IPC registry: {e}"
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Register the port currently selected in the "Add port" form.
    pub fn register_serial(&mut self) {
        let Some(idx) = self.new_port_selection else {
            return;
        };
        let unconfigured: Vec<crate::port_registry::AvailablePort> = self
            .unconfigured_available_ports()
            .into_iter()
            .cloned()
            .collect();
        let Some(port) = unconfigured.get(idx).cloned() else {
            return;
        };

        let adapter_type = self.new_port_type;
        self.start_serial_port(port, adapter_type, true);
        self.new_port_selection = None;
    }

    /// Unregister a serial port and stop its connection loop. Also removes
    /// it from the persistent registry so it won't auto-register on next
    /// startup.
    /// Send an IPC call to a device. Returns a oneshot receiver for the
    /// result. The caller awaits or polls the receiver.
    pub fn ipc_call(
        &self,
        device_id: DeviceId,
        resource: &str,
        method: &str,
        args: ipc_runtime::IpcValue,
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<usb::IpcCallResult, String>>,
        String,
    > {
        let dev = self
            .devices
            .get(&device_id)
            .ok_or("device not found")?;
        let registry = dev
            .ipc_registry
            .as_ref()
            .ok_or("no IPC registry loaded for this device")?;

        let encoded = registry
            .encode_call(resource, method, args)
            .map_err(|e| e.to_string())?;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = self.cmd_tx.send(crate::bridge::Command::IpcCall {
            device_id,
            call: encoded,
            reply: tx,
        });
        Ok(rx)
    }

    pub fn unregister_serial(&mut self, index: usize) {
        if index >= self.serial_ports.len() {
            return;
        }

        let _ = self.cmd_tx.send(crate::bridge::Command::UnregisterSerial { index });

        // Remove any associated device.
        if let Some(device_id) = self.serial_ports[index].device_id {
            self.devices.remove(&device_id);
        }

        // Remove any open tiles referencing this port.
        let to_remove: Vec<egui_tiles::TileId> = self
            .tree
            .tiles
            .iter()
            .filter_map(|(tile_id, tile)| match tile {
                egui_tiles::Tile::Pane(Pane::SerialAdapter(i))
                | egui_tiles::Tile::Pane(Pane::SerialAdapterLogs(i))
                | egui_tiles::Tile::Pane(Pane::SerialAdapterControl(i))
                    if *i == index =>
                {
                    Some(*tile_id)
                }
                _ => None,
            })
            .collect();
        for tile_id in to_remove {
            self.tree.remove_recursively(tile_id);
        }

        let removed = self.serial_ports.remove(index);
        if let Some(identity) = removed.identity {
            self.port_registry.remove(&identity);
            self.port_registry.save();
        }
    }

    /// Start a build: create state, open a panel, send the command to the bridge.
    pub fn start_build(&mut self) {
        let config_name = self.build_configs.get(self.selected_config).cloned();
        let board_name = self.build_boards.get(self.selected_board).cloned();
        let layout_name = self.build_layouts.get(self.selected_layout).cloned();

        let (Some(config), Some(board), Some(layout)) =
            (config_name, board_name, layout_name)
        else {
            return;
        };

        let build_id = self.next_build_id();
        let build_config = BuildConfig {
            config: config.clone(),
            board: board.clone(),
            layout: layout.clone(),
        };

        self.builds.insert(
            build_id,
            BuildHandle {
                id: build_id,
                config: build_config.clone(),
                status: BuildStatus::Running {
                    stage: "starting".into(),
                    detail: String::new(),
                },
                log: vec![format!("Building {config} (board={board}, layout={layout})")],
            },
        );

        // Open a build output panel.
        let pane = Pane::Build(build_id);
        let tile = self.tree.tiles.insert_pane(pane);
        if let Some(root) = self.tree.root() {
            if let Some(egui_tiles::Tile::Container(container)) =
                self.tree.tiles.get_mut(root)
            {
                container.add_child(tile);
            } else {
                let new_root = self
                    .tree
                    .tiles
                    .insert_horizontal_tile(vec![root, tile]);
                self.tree.root = Some(new_root);
            }
        } else {
            let tab = self.tree.tiles.insert_tab_tile(vec![tile]);
            self.tree.root = Some(tab);
        }

        let out_dir = self.firmware_dir.parent().unwrap_or(&self.firmware_dir).join("build");
        let _ = std::fs::create_dir_all(&out_dir);
        let out_path = out_dir.join(format!("{config}.tfw"));

        let _ = self.cmd_tx.send(crate::bridge::Command::Build {
            build_id,
            firmware_dir: self.firmware_dir.clone(),
            config: format!("apps/{config}.ncl"),
            board: format!("boards/{board}.ncl"),
            layout: format!("layouts/{layout}.ncl"),
            out: out_path,
        });
    }

    /// Launch an emulator for the given firmware.
    pub fn run_emulator(&mut self, fw_id: FirmwareId) {
        let Some(fw) = self.firmware.get(&fw_id) else {
            return;
        };
        let Some(tfw_path) = fw.path.clone() else {
            // Builtin entries (e.g. the embedded stub) have no on-disk
            // representation, so they can't be launched in the emulator.
            return;
        };

        let _ = self.cmd_tx.send(crate::bridge::Command::RunEmulator {
            firmware_id: fw_id,
            tfw_path,
        });
    }
}

// ── Firmware database ──────────────────────────────────────────────────

/// The firmware database directory: `~/.rcard/firmware/`.
fn firmware_db_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".rcard").join("firmware")
}

/// Compute the database path for a firmware archive.
/// Uses `<build_id>.tfw` if a build_id exists, otherwise the filename.
fn firmware_db_path(metadata: &tfw::archive::TfwMetadata) -> Result<PathBuf, String> {
    let dir = firmware_db_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create firmware db dir: {e}"))?;

    let filename = match metadata.build.as_ref() {
        Some(build) => format!("{}.tfw", build.build_id),
        None => "unknown.tfw".into(),
    };
    Ok(dir.join(filename))
}

// ── Nickel discovery ──────────────────────────────────────────────────

/// Discover .ncl file names (without extension) in firmware_dir/subdir.
fn discover_ncl_names(firmware_dir: &Path, subdir: &str) -> Vec<String> {
    let dir = if subdir.is_empty() {
        firmware_dir.to_path_buf()
    } else {
        firmware_dir.join(subdir)
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_suffix(".ncl").map(|s| s.to_string())
        })
        .collect();
    names.sort();
    names
}
