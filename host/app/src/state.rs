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
///
/// `Ipc` is the abstract "IPC tunnel reachable" signal regardless of wire.
/// The specific wire (USB / USART2 / future BLE) is recorded separately on
/// each `AdapterHandle::ipc_transport` so callers can both decide on
/// presence (`has_capability(Ipc)`) and display the actual transport
/// (`flash_method_for_device` walks the contributing adapters).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnownCapability {
    SifliDebug,
    Ipc,
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
    /// Known capabilities on this device, indexed by the adapter that
    /// contributed them. Per-adapter so they revoke automatically when the
    /// adapter goes away — `AdapterRemoved` just drops the entry, no
    /// recompute needed. Use [`DeviceHandle::has_capability`] to check
    /// presence across all adapters.
    pub capabilities: HashMap<AdapterId, HashSet<KnownCapability>>,
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
        firmware_id: Option<FirmwareId>,
    ) -> Self {
        DeviceHandle {
            id,
            name,
            kind,
            phase: DevicePhase::Unknown,
            uid: None,
            capabilities: HashMap::new(),
            adapter_ids: Vec::new(),
            firmware_id,
            log_buffer: Vec::new(),
            ipc_registry: None,
        }
    }

    /// True if any currently-attached adapter on this device provides
    /// the given capability.
    pub fn has_capability(&self, cap: KnownCapability) -> bool {
        self.capabilities.values().any(|set| set.contains(&cap))
    }

    pub fn push_log(&mut self, log: Log) {
        // Sorted insertion. When both logs carry a device-side tick,
        // compare by tick — immune to USB-serial buffering jitter.
        // Fall back to host `received_at` when either side lacks a
        // device tick (early boot, auxiliary streams, etc.).
        let idx = self.log_buffer.partition_point(|existing| {
            match (existing.device_tick, log.device_tick) {
                (Some(a), Some(b)) => a <= b,
                _ => existing.received_at <= log.received_at,
            }
        });
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
    /// If this adapter contributes `KnownCapability::Ipc`, the transport
    /// label (e.g. `"usb"`, `"usart2"`) and its priority — same priority
    /// the bridge's `crate::ipc::pick` uses when multiple Ipc adapters
    /// are attached. `None` for non-IPC adapters (SifliDebug, USART1).
    pub ipc_transport: Option<(&'static str, u8)>,
}

impl AdapterHandle {
    pub fn new(
        id: AdapterId,
        display_name: String,
        ipc_transport: Option<(&'static str, u8)>,
    ) -> Self {
        AdapterHandle {
            id,
            display_name,
            ipc_transport,
        }
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

/// Current status of a build. Coarse-grained — drives the sidebar button
/// and status badge. Fine-grained progress lives in [`BuildHandle::phase`]
/// and the resource collections.
pub enum BuildStatus {
    Running,
    Succeeded {
        tfw_path: PathBuf,
        firmware_id: Option<FirmwareId>,
    },
    Failed {
        error: String,
    },
}

/// Major phases of the build pipeline. Mirrors
/// [`tfw::build::BuildState`] but lives in the GUI crate so we don't drag
/// the full dependency in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipelinePhase {
    Planning,
    CompilingTasks,
    Organizing { regions_placed: usize },
    CompilingApp,
    ExtractingMetadata,
    Packing,
    Done,
}

impl PipelinePhase {
    /// Stable 0-based index for ordering / progress.
    pub fn order(&self) -> u8 {
        match self {
            PipelinePhase::Planning => 0,
            PipelinePhase::CompilingTasks => 1,
            PipelinePhase::Organizing { .. } => 2,
            PipelinePhase::CompilingApp => 3,
            PipelinePhase::ExtractingMetadata => 4,
            PipelinePhase::Packing => 5,
            PipelinePhase::Done => 6,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            PipelinePhase::Planning => "PLAN",
            PipelinePhase::CompilingTasks => "TASKS",
            PipelinePhase::Organizing { .. } => "ORGANIZE",
            PipelinePhase::CompilingApp => "APP",
            PipelinePhase::ExtractingMetadata => "METADATA",
            PipelinePhase::Packing => "PACK",
            PipelinePhase::Done => "DONE",
        }
    }
}

/// Which semantic category a crate belongs to in the UI grouping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrateKind {
    Bootloader,
    Kernel,
    Task,
    /// Privileged modules that aren't tasks (reserved; emitted when the
    /// build pipeline starts distinguishing them).
    Sysmodule,
    /// Host-side tooling (schema dumper, metadata scrapers). Runs on the
    /// host, not deployed to the device.
    HostCrate,
}

/// Build-time state machine for an embedded crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrateBuildState {
    /// Not yet started. Shown faded with "queued".
    Queued,
    /// Cargo is compiling the crate to a relocatable object.
    Building,
    /// Cargo finished producing the relocatable object; awaiting the
    /// batched Measuring pass. A crate can sit here for a while if
    /// sibling crates are still compiling.
    Compiled,
    /// Re-linking at temporary addresses to measure region sizes.
    Measuring,
    /// Linking at final memory addresses.
    Linking,
    /// Final ELF produced.
    Linked,
    /// Cargo or linker reported an error. The crate row stays expanded.
    Failed,
}

/// Build-time state machine for a host-side crate (schema_dump etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCrateBuildState {
    Queued,
    Building,
    Running,
    Done,
    Failed,
}

/// An IPC resource this crate *serves*. Populated from `tfw`'s
/// `IpcMetadataBundle.servers[task].serves` + the matching
/// `resources[name].methods.len()`.
#[derive(Clone, Debug)]
pub struct ProvidedResource {
    /// Resource trait name, e.g. `"Log"`.
    pub resource: String,
    pub method_count: usize,
}

/// An IPC resource this crate *consumes* from another server task.
/// Populated by walking `TaskConfig::depends_on` and cross-referencing
/// each dependency's `serves` list.
#[derive(Clone, Debug)]
pub struct UsedResource {
    /// Task name that serves this resource, e.g. `"log_server"`.
    pub server_task: String,
    /// Resource trait name, e.g. `"Log"`.
    pub resource: String,
}

/// Per-crate build progress. Covers both embedded and host crates — the
/// `kind` discriminates. Populated progressively from bridge events.
pub struct CrateProgress {
    pub name: String,
    pub kind: CrateKind,
    /// For [`CrateKind::HostCrate`] the embedded state is re-purposed by
    /// mapping via `host_state_to_embedded`; the authoritative value is
    /// [`CrateProgress::host_state`] when `kind == HostCrate`.
    pub state: CrateBuildState,
    pub host_state: Option<HostCrateBuildState>,
    /// Scheduling priority (0 = highest). Known from config, set at
    /// first event that references this crate.
    pub priority: Option<u32>,
    /// Measured section sizes, in bytes, keyed by region name.
    pub sizes: std::collections::BTreeMap<String, u64>,
    /// Total ELF size after link (sum of all regions) — populated on Linked.
    pub total_size: Option<u64>,
    /// Raw cargo JSON messages (ndjson lines). Stored for archive
    /// persistence. The renderer reads from the pre-decoded `cargo_summary`
    /// instead of parsing these every frame.
    pub cargo_messages: Vec<String>,
    /// Pre-decoded summary of `cargo_messages`. Updated incrementally
    /// as messages arrive; the renderer reads this directly.
    pub cargo_summary: CargoSummary,
    /// Error message if this crate failed.
    pub error: Option<String>,
    /// IPC resources this crate serves — drives the "provides" row in
    /// the dropdown body.
    pub provides: Vec<ProvidedResource>,
    /// IPC resources this crate consumes — drives the "uses" row.
    pub uses: Vec<UsedResource>,
}

/// Pre-decoded cargo message summary for a single crate. Populated
/// incrementally as messages arrive so the renderer never parses JSON.
#[derive(Clone, Debug, Default)]
pub struct CargoSummary {
    pub deps_fresh: usize,
    pub deps_compiled: Vec<String>,
    pub diagnostics: Vec<CargoDiagnostic>,
    pub raw_errors: Vec<String>,
}

/// A single compiler diagnostic, pre-decoded from cargo JSON.
#[derive(Clone, Debug)]
pub struct CargoDiagnostic {
    pub level: CargoDiagLevel,
    pub message: String,
    pub code: Option<String>,
    pub location: Option<String>,
    pub rendered: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CargoDiagLevel {
    Error,
    Warning,
    Note,
    Help,
}

impl CrateProgress {
    pub fn new(name: String, kind: CrateKind) -> Self {
        Self {
            name,
            kind,
            state: CrateBuildState::Queued,
            host_state: None,
            priority: None,
            sizes: std::collections::BTreeMap::new(),
            total_size: None,
            cargo_messages: Vec::new(),
            cargo_summary: CargoSummary::default(),
            error: None,
            provides: Vec::new(),
            uses: Vec::new(),
        }
    }

    /// Push a raw cargo JSON message, decoding it into the summary
    /// at the same time.
    pub fn push_cargo_message(&mut self, raw: String) {
        if let Ok(msg) = serde_json::from_str::<escargot::format::Message>(&raw) {
            match msg {
                escargot::format::Message::CompilerMessage(cm) => {
                    let d = &cm.message;
                    let level = match d.level {
                        escargot::format::diagnostic::DiagnosticLevel::Ice
                        | escargot::format::diagnostic::DiagnosticLevel::Error => {
                            CargoDiagLevel::Error
                        }
                        escargot::format::diagnostic::DiagnosticLevel::Warning => {
                            CargoDiagLevel::Warning
                        }
                        escargot::format::diagnostic::DiagnosticLevel::Help => {
                            CargoDiagLevel::Help
                        }
                        _ => CargoDiagLevel::Note,
                    };
                    self.cargo_summary.diagnostics.push(CargoDiagnostic {
                        level,
                        message: d.message.trim().to_string(),
                        code: d.code.as_ref().map(|c| c.code.to_string()),
                        location: d
                            .spans
                            .iter()
                            .find(|s| s.is_primary)
                            .map(|s| {
                                let path = s.file_name.display();
                                format!("{path}:{}:{}", s.line_start, s.column_start)
                            }),
                        rendered: d
                            .rendered
                            .as_deref()
                            .unwrap_or_else(|| d.message.as_ref())
                            .trim()
                            .to_string(),
                    });
                }
                escargot::format::Message::CompilerArtifact(ca) => {
                    if !ca.target.kind.iter().any(|k| k.as_ref() == "custom-build") {
                        if ca.fresh {
                            self.cargo_summary.deps_fresh += 1;
                        } else {
                            self.cargo_summary
                                .deps_compiled
                                .insert(0, ca.target.name.to_string());
                        }
                    }
                }
                _ => {}
            }
        } else if !raw.is_empty() {
            self.cargo_summary.raw_errors.push(raw.clone());
        }
        self.cargo_messages.push(raw);
    }
}

/// A memory allocation emitted by the layout solver.
#[derive(Clone, Debug)]
pub struct MemoryAllocation {
    /// Place where this allocation actually landed.
    pub place: String,
    /// Crate name / "kernel" / "bootloader" owning this region.
    pub owner: String,
    /// Region name: "code", "data", "stack", etc.
    pub region: String,
    pub base: u64,
    pub size: u64,
    /// Place originally requested (differs from `place` if overflowed).
    pub requested_place: String,
}

/// A physical memory device available on the board — flash bank, sram
/// region, etc. Distinct from a `Place`: a place is a *request*
/// (logical name used by a crate to ask for storage); a memory device
/// is a *destination* (actual hardware with a fixed capacity and
/// address mapping). The memory map renders bars per device.
#[derive(Clone, Debug)]
pub struct MemoryDevice {
    pub name: String,
    /// Total capacity in bytes.
    pub size: u64,
    /// CPU-visible `(address, size)` pairs this device appears at.
    pub mappings: Vec<(u64, u64)>,
}

impl MemoryDevice {
    /// Does the given address fall inside any of this device's CPU
    /// mappings?
    pub fn contains_address(&self, addr: u64) -> bool {
        self.mappings
            .iter()
            .any(|(start, size)| addr >= *start && addr < start.saturating_add(*size))
    }
}

/// State of the output firmware image across the build.
#[derive(Clone, Debug)]
pub enum ImageProgress {
    /// No image work yet.
    None,
    /// Flat binary was assembled in memory.
    Assembled { size: u64 },
    /// .tfw archive was written to disk.
    Archived { size: u64, path: PathBuf },
}

/// An IPC resource reachable from this firmware — the Resources card's
/// data model. Populated from `TfwMetadata.ipc.resources` +
/// `ipc.servers`. One entry per distinct resource trait.
#[derive(Clone, Debug)]
pub struct ResourceSummary {
    /// Resource trait name, e.g. `"Log"`.
    pub name: String,
    /// Task that implements this resource ("log_server"), or empty if
    /// no server was declared for it.
    pub provider_task: String,
    /// Method signatures in order, e.g. `"log(msg: &str)"`.
    pub methods: Vec<String>,
}

impl ResourceSummary {
    /// Derive the Resources card data from a `tfw` IPC metadata
    /// bundle. Used by both the live-build path (bridge forwards
    /// `BuildEvent::IpcMetadata`) and the loaded-firmware path
    /// (`snapshot_from_firmware`) so the two sources converge on the
    /// same `Vec<ResourceSummary>`.
    pub fn list_from_bundle(bundle: &tfw::ipc_metadata::IpcMetadataBundle) -> Vec<Self> {
        let provider_by_resource: HashMap<&str, &str> = bundle
            .servers
            .iter()
            .flat_map(|(task, server)| {
                server
                    .serves
                    .iter()
                    .map(move |r| (r.as_str(), task.as_str()))
            })
            .collect();
        bundle
            .resources
            .iter()
            .map(|(name, res)| {
                let methods = res
                    .methods
                    .iter()
                    .map(|m| {
                        let params = m
                            .params
                            .iter()
                            .map(|p| format!("{}: {}", p.name, p.ty))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let ret = m
                            .return_type
                            .as_deref()
                            .map(|t| format!(" -> {t}"))
                            .unwrap_or_default();
                        format!("{}({params}){ret}", m.name)
                    })
                    .collect();
                ResourceSummary {
                    name: name.clone(),
                    provider_task: provider_by_resource
                        .get(name.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    methods,
                }
            })
            .collect()
    }
}

/// A build tracked by the app. Holds everything needed to render the
/// unified Build/Firmware panel — coarse status, live pipeline phase,
/// per-crate progress, memory allocations, image state, and a
/// free-form pipeline log.
pub struct BuildHandle {
    pub id: BuildId,
    pub config: BuildConfig,
    /// Resolved app name from the Nickel config (e.g. "rcard").
    /// Populated from `AppConfig.name` for snapshots, or from the
    /// `ConfigResolved` event for live builds. Falls back to
    /// `config.config` until the resolved name arrives.
    pub name: Option<String>,
    pub status: BuildStatus,
    /// UUID (UUIDv4 string) generated during Planning. Populated by
    /// the `BuildUuid` event for live builds, or from
    /// `TfwMetadata.build.build_id` for loaded snapshots.
    pub uuid: Option<String>,
    /// Current phase of the build pipeline. None before the first
    /// Build event arrives.
    pub phase: Option<PipelinePhase>,
    /// All crates we've seen — embedded and host — in event-arrival order.
    pub crates: Vec<CrateProgress>,
    /// All memory allocations reported by the layout solver.
    pub allocations: Vec<MemoryAllocation>,
    /// Capacity (in bytes) of each named place. Populated at the start
    /// of the build from the resolved config. Used as a secondary
    /// overflow check; the primary memory map is driven by
    /// [`Self::memories`].
    pub place_capacities: HashMap<String, u64>,
    /// Physical memory devices the firmware may target. Drives the
    /// memory map card.
    pub memories: Vec<MemoryDevice>,
    /// IPC resources discovered in the firmware. Drives the Resources
    /// card. Populated for loaded snapshots from
    /// `TfwMetadata.ipc.resources`; left empty for live builds until
    /// the ExtractingMetadata stage finishes (not wired yet).
    pub resources: Vec<ResourceSummary>,
    /// Output image state.
    pub image: ImageProgress,
    /// Free-form build log — stage events, pipeline messages. Cargo
    /// output lives in each crate's `cargo_messages` instead.
    pub log: Vec<String>,
    pub started_at: std::time::Instant,
    pub finished_at: Option<std::time::Instant>,
}

impl BuildHandle {
    /// Look up or create a crate progress entry by name.
    pub fn crate_mut(&mut self, name: &str, kind: CrateKind) -> &mut CrateProgress {
        if let Some(idx) = self.crates.iter().position(|c| c.name == name) {
            &mut self.crates[idx]
        } else {
            self.crates.push(CrateProgress::new(name.to_string(), kind));
            self.crates.last_mut().unwrap()
        }
    }

    /// Total elapsed duration, live while running and frozen on finish.
    pub fn elapsed(&self) -> std::time::Duration {
        self.finished_at
            .unwrap_or_else(std::time::Instant::now)
            .saturating_duration_since(self.started_at)
    }

    /// Categorised view of crates — bootloader, kernel, tasks, sysmods,
    /// host crates. Useful for grouped rendering.
    pub fn crates_by_kind(&self, kind: CrateKind) -> impl Iterator<Item = &CrateProgress> {
        self.crates.iter().filter(move |c| c.kind == kind)
    }

    /// Build a snapshot `BuildHandle` that describes a finished firmware
    /// archive — driven by the data already present in its
    /// `TfwMetadata`. The `id` field is a placeholder `BuildId(0)`;
    /// the caller ([`AppState::build_for_firmware`]) stamps a real
    /// `BuildId` before inserting into `state.builds`.
    pub fn snapshot_from_firmware(fw: &FirmwareHandle) -> Self {
        use tfw::config::TaskConfig;

        let now = std::time::Instant::now();
        // If the archive recorded a build duration, walk `started_at`
        // back by that amount so `BuildHandle::elapsed()` returns the
        // real value. Otherwise collapse to zero and let the panel
        // hide the field.
        let (started_at, finished_at) = match fw
            .metadata
            .build
            .as_ref()
            .and_then(|b| b.build_duration_ms)
        {
            Some(ms) => (now - std::time::Duration::from_millis(ms), Some(now)),
            None => (now, Some(now)),
        };
        let mut crates: Vec<CrateProgress> = Vec::new();
        let mut place_capacities: HashMap<String, u64> = HashMap::new();
        let mut memories: Vec<MemoryDevice> = Vec::new();
        let mut allocations: Vec<MemoryAllocation> = Vec::new();
        let mut resources: Vec<ResourceSummary> = Vec::new();

        let app_name = fw
            .metadata
            .build
            .as_ref()
            .map(|b| b.name.clone())
            .unwrap_or_else(|| fw.display_name());
        let config_stem = fw
            .metadata
            .build
            .as_ref()
            .map(|b| b.config.clone())
            .unwrap_or_else(|| app_name.clone());
        let board = fw
            .metadata
            .build
            .as_ref()
            .map(|b| b.board.clone())
            .unwrap_or_else(|| "?".into());
        let layout_name = fw
            .metadata
            .build
            .as_ref()
            .map(|b| b.layout.clone())
            .unwrap_or_else(|| "?".into());

        if let Some(config) = &fw.metadata.config {
            // Synthesise kernel.
            crates.push(synthesise_linked_crate(
                &config.kernel.crate_info.package.name,
                CrateKind::Kernel,
                Some(0),
            ));
            // Synthesise bootloader (if present).
            if let Some(bl) = &config.bootloader {
                crates.push(synthesise_linked_crate(
                    &bl.crate_info.package.name,
                    CrateKind::Bootloader,
                    Some(0),
                ));
            }
            // Flatten the task tree. Each task carries its direct deps
            // along so we can derive `uses` chips. `seen` dedups the
            // graph since a task can appear under multiple parents.
            fn walk_tasks(
                task: &TaskConfig,
                out: &mut Vec<CrateProgress>,
                seen: &mut std::collections::HashSet<String>,
            ) {
                let name = &task.crate_info.package.name;
                if !seen.insert(name.clone()) {
                    return;
                }
                let kind = if name.starts_with("sysmodule_") {
                    CrateKind::Sysmodule
                } else {
                    CrateKind::Task
                };
                let mut c = synthesise_linked_crate(
                    name,
                    kind,
                    Some(task.priority),
                );
                // `uses` = direct dependencies' names; resource names
                // get filled in later once we have IpcMetadata.
                for dep in &task.depends_on {
                    c.uses.push(UsedResource {
                        server_task: dep.crate_info.package.name.clone(),
                        resource: String::new(),
                    });
                }
                out.push(c);
                for dep in &task.depends_on {
                    walk_tasks(dep, out, seen);
                }
            }
            let mut seen_tasks = std::collections::HashSet::new();
            for task in &config.entries {
                walk_tasks(task, &mut crates, &mut seen_tasks);
            }

            // Fill in per-crate provides/uses from IPC metadata.
            if let Some(ipc) = &fw.metadata.ipc {
                resources = ResourceSummary::list_from_bundle(ipc);
                apply_ipc_to_crates(&mut crates, ipc);
            }

            // Capacities from the resolved places.
            for (name, place) in &config.places {
                place_capacities.insert(name.clone(), place.size);
            }
            // Physical memory devices (actual hardware banks).
            for (name, mem) in &config.memory {
                memories.push(MemoryDevice {
                    name: name.clone(),
                    size: mem.size,
                    mappings: mem
                        .mappings
                        .iter()
                        .map(|m| (m.address, m.size))
                        .collect(),
                });
            }
        }

        // Prefer the archive's solved allocations — those are the
        // authoritative sizes the linker settled on. Older archives
        // that predate allocation persistence get an empty list;
        // the memory map card shows capacities only in that case.
        if let Some(build) = &fw.metadata.build {
            for a in &build.allocations {
                let alloc = MemoryAllocation {
                    place: a.place.clone(),
                    owner: a.owner.clone(),
                    region: a.region.clone(),
                    base: a.base,
                    size: a.size,
                    requested_place: a.requested_place.clone(),
                };
                // Dedup by (owner, region) — same as live path.
                if let Some(existing) = allocations.iter_mut().find(|x| {
                    x.owner == alloc.owner && x.region == alloc.region
                }) {
                    *existing = alloc;
                } else {
                    allocations.push(alloc);
                }
            }
            // Populate per-crate cargo messages from the archive so
            // loaded firmware shows the same build output as live.
            for msg in &build.cargo_messages {
                if let Some(c) = crates.iter_mut().find(|c| c.name == msg.crate_name) {
                    c.push_cargo_message(msg.raw.clone());
                }
            }
        }

        let image = if let Some(path) = &fw.path {
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            ImageProgress::Archived {
                size,
                path: path.clone(),
            }
        } else {
            ImageProgress::None
        };

        let uuid = fw.metadata.build.as_ref().map(|b| b.build_id.clone());

        BuildHandle {
            // Placeholder — `build_for_firmware` assigns a real
            // `BuildId` immediately after construction.
            id: BuildId(0),
            config: BuildConfig {
                config: config_stem,
                board,
                layout: layout_name,
            },
            name: Some(app_name),
            status: BuildStatus::Succeeded {
                tfw_path: fw.path.clone().unwrap_or_default(),
                firmware_id: Some(fw.id),
            },
            uuid,
            phase: Some(PipelinePhase::Done),
            crates,
            allocations,
            place_capacities,
            memories,
            resources,
            image,
            log: Vec::new(),
            started_at,
            finished_at,
        }
    }
}

fn synthesise_linked_crate(
    name: &str,
    kind: CrateKind,
    priority: Option<u32>,
) -> CrateProgress {
    let mut c = CrateProgress::new(name.to_string(), kind);
    c.state = CrateBuildState::Linked;
    c.priority = priority;
    c
}

/// Map a `tfw::build::CrateKind` to a GUI `CrateKind`, promoting
/// `sysmodule_*` names to `Sysmodule`.
fn classify_resolved_kind(name: &str, tfw_kind: tfw::build::CrateKind) -> CrateKind {
    match tfw_kind {
        tfw::build::CrateKind::Kernel => CrateKind::Kernel,
        tfw::build::CrateKind::Bootloader => CrateKind::Bootloader,
        tfw::build::CrateKind::Task => {
            if name.starts_with("sysmodule_") {
                CrateKind::Sysmodule
            } else {
                CrateKind::Task
            }
        }
    }
}

/// Apply IPC metadata to existing crate entries — fills in `provides`
/// and expands `uses` from task-level deps to resource-level deps.
/// Shared between live builds (`BuildResources` event) and snapshots
/// (`snapshot_from_firmware`).
fn apply_ipc_to_crates(
    crates: &mut [CrateProgress],
    bundle: &tfw::ipc_metadata::IpcMetadataBundle,
) {
    // task_name → list of resource names it serves.
    let serves_by_task: HashMap<&str, &[String]> = bundle
        .servers
        .iter()
        .map(|(task, server)| (task.as_str(), server.serves.as_slice()))
        .collect();
    // resource name → method count.
    let method_count: HashMap<&str, usize> = bundle
        .resources
        .iter()
        .map(|(name, res)| (name.as_str(), res.methods.len()))
        .collect();

    // First pass: fill `provides`.
    for c in crates.iter_mut() {
        if let Some(serves) = serves_by_task.get(c.name.as_str()) {
            // Only add if not already populated (idempotent for snapshots
            // that may call this after initial population).
            if c.provides.is_empty() {
                for resource in serves.iter() {
                    c.provides.push(ProvidedResource {
                        resource: resource.clone(),
                        method_count: method_count
                            .get(resource.as_str())
                            .copied()
                            .unwrap_or(0),
                    });
                }
            }
        }
    }

    // Second pass: expand `uses` — each dependency task name gets
    // replaced by one entry per resource that task serves.
    for c in crates.iter_mut() {
        let mut expanded: Vec<UsedResource> = Vec::new();
        for u in std::mem::take(&mut c.uses) {
            if let Some(serves) = serves_by_task.get(u.server_task.as_str()) {
                for resource in serves.iter() {
                    expanded.push(UsedResource {
                        server_task: u.server_task.clone(),
                        resource: resource.clone(),
                    });
                }
            } else {
                expanded.push(u);
            }
        }
        c.uses = expanded;
    }
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
    /// Device has an IPC tunnel — flash stub via the existing firmware's
    /// IPC, then flash target via stub. `transport` is the wire the IPC
    /// will actually use (`"usb"`, `"usart2"`, …) — the picker resolves
    /// the same way the bridge's `crate::ipc::pick` does, so the label is
    /// truthful, not just a guess.
    Ipc { transport: &'static str },
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
    /// Handle to the tokio runtime. Used to spawn async work (e.g.
    /// flash-smoke IPC calls) from the sync egui event loop.
    pub tokio_handle: tokio::runtime::Handle,
}

impl AppState {
    pub fn new(
        cmd_tx: tokio::sync::mpsc::UnboundedSender<crate::bridge::Command>,
        event_rx: crossbeam_channel::Receiver<crate::bridge::Event>,
        tokio_handle: tokio::runtime::Handle,
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
            tokio_handle,
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

    /// Clean up devices that lost all adapters: delete ephemeral/emulator
    /// devices, reset persistent devices to phase Unknown.
    fn cleanup_orphaned_devices(&mut self) {
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
        for dev in self.devices.values_mut() {
            if dev.kind == DeviceKind::Persistent && dev.adapter_ids.is_empty() {
                dev.phase = DevicePhase::Unknown;
            }
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
    ///
    /// For `FlashMethod::Ipc`, walks every adapter that contributes the
    /// `Ipc` capability on this device and picks the highest-priority
    /// `ipc_transport` — same ranking the bridge applies in
    /// `crate::ipc::pick`, so the displayed transport matches the wire
    /// that an actual IPC call would land on.
    pub fn flash_method_for_device(&self, device_id: DeviceId) -> Option<FlashMethod> {
        let dev = self.devices.get(&device_id)?;
        // TODO: temporarily forcing SifliDebug to diagnose IPC-over-USB
        // flash timeout. Remove this override once resolved.
        if dev.has_capability(KnownCapability::SifliDebug) {
            Some(FlashMethod::SifliDebug)
        } else if dev.has_capability(KnownCapability::Ipc) {
            let transport = dev
                .capabilities
                .iter()
                .filter(|(_, set)| set.contains(&KnownCapability::Ipc))
                .filter_map(|(adapter_id, _)| self.adapters.get(adapter_id))
                .filter_map(|a| a.ipc_transport)
                .max_by_key(|(_, prio)| *prio)
                .map(|(label, _)| label)
                .unwrap_or("?");
            Some(FlashMethod::Ipc { transport })
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

    /// Resolve a `FirmwareId` to a `BuildId` — reusing an existing
    /// `BuildHandle` if one already references this firmware (e.g. a
    /// live build that just completed), otherwise synthesising a
    /// snapshot from the archive and stashing it in `self.builds`.
    ///
    /// This is the unification pivot: every panel in the system deals
    /// in `BuildId`; firmware-on-disk and live-build lookups converge
    /// here.
    pub fn build_for_firmware(&mut self, fw_id: FirmwareId) -> Option<BuildId> {
        // Reuse an existing BuildHandle if one already points at this
        // firmware — live builds that completed set
        // `status.Succeeded.firmware_id`.
        for (bid, b) in &self.builds {
            if let BuildStatus::Succeeded {
                firmware_id: Some(f),
                ..
            } = &b.status
            {
                if *f == fw_id {
                    return Some(*bid);
                }
            }
        }
        let fw = self.firmware.get(&fw_id)?;
        let mut handle = BuildHandle::snapshot_from_firmware(fw);
        let id = self.next_build_id();
        handle.id = id;
        self.builds.insert(id, handle);
        Some(id)
    }

    /// Open a firmware pane in the tile tree. Synthesises a
    /// `BuildHandle` for the archive if one doesn't already exist,
    /// then opens / focuses `Pane::Firmware(build_id)`.
    pub fn open_firmware(&mut self, id: FirmwareId) {
        let Some(build_id) = self.build_for_firmware(id) else {
            return;
        };
        let target = Pane::Firmware(build_id);

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
                    ipc_transport,
                } => {
                    self.adapters.insert(
                        adapter_id,
                        AdapterHandle::new(adapter_id, display_name, ipc_transport),
                    );
                }
                crate::bridge::Event::AdapterRemoved { adapter_id } => {
                    self.adapters.remove(&adapter_id);
                    // Defensive: strip this adapter from any devices that
                    // still reference it (covers abrupt port loss where
                    // AdapterUnbound didn't fire first).
                    for dev in self.devices.values_mut() {
                        dev.adapter_ids.retain(|id| *id != adapter_id);
                        dev.capabilities.remove(&adapter_id);
                    }
                    self.cleanup_orphaned_devices();
                }
                crate::bridge::Event::DeviceCreated {
                    device_id,
                    name,
                    kind,
                    firmware_id,
                } => {
                    // Idempotent: if the device already exists (e.g. a
                    // second adapter resolved the same chip UID), this is
                    // a no-op. Adapter bindings arrive via AdapterBound.
                    if !self.devices.contains_key(&device_id) {
                        let dev = DeviceHandle::new(
                            device_id,
                            name,
                            kind,
                            firmware_id,
                        );
                        let is_emulator = dev.kind == DeviceKind::Emulator;
                        self.devices.insert(device_id, dev);
                        // Auto-open emulator devices on creation.
                        if is_emulator {
                            self.open_device(device_id);
                        }
                    }
                }
                crate::bridge::Event::DeviceDeleted { device_id } => {
                    self.remove_device(device_id);
                }
                crate::bridge::Event::AdapterBound {
                    adapter_id,
                    device_id,
                    capabilities,
                } => {
                    if let Some(dev) = self.devices.get_mut(&device_id) {
                        if !dev.adapter_ids.contains(&adapter_id) {
                            dev.adapter_ids.push(adapter_id);
                        }
                        let cap_set: HashSet<KnownCapability> =
                            capabilities.into_iter().collect();
                        dev.capabilities
                            .entry(adapter_id)
                            .or_default()
                            .extend(cap_set);
                    }
                    // Link serial config to the device if applicable.
                    for cfg in &mut self.serial_ports {
                        if cfg.device_id.is_none()
                            && cfg.status == SerialPortStatus::DeviceDetected
                        {
                            cfg.device_id = Some(device_id);
                            break;
                        }
                    }
                }
                crate::bridge::Event::AdapterUnbound {
                    adapter_id,
                    device_id,
                } => {
                    if let Some(dev) = self.devices.get_mut(&device_id) {
                        dev.adapter_ids.retain(|id| *id != adapter_id);
                        dev.capabilities.remove(&adapter_id);
                    }
                    // Clear serial config if it pointed at the unbound device.
                    for cfg in &mut self.serial_ports {
                        if cfg.device_id == Some(device_id) {
                            cfg.device_id = None;
                        }
                    }
                    self.cleanup_orphaned_devices();
                }
                crate::bridge::Event::Log { device, log } => {
                    if let Some(dev) = self.devices.get_mut(&device) {
                        dev.push_log(log);
                    }
                }
                crate::bridge::Event::BuildPhase { build_id, phase } => {
                    if let Some(build) = self.builds.get_mut(&build_id) {
                        build.log.push(format!("stage: {}", phase.label()));
                        build.phase = Some(phase);
                    }
                }
                crate::bridge::Event::BuildConfigResolved {
                    build_id,
                    uuid,
                    name,
                    memories,
                    place_capacities,
                    tasks,
                } => {
                    if let Some(build) = self.builds.get_mut(&build_id) {
                        build.uuid = Some(uuid);
                        build.name = Some(name);
                        build.memories = memories;
                        for (name, size) in place_capacities {
                            build.place_capacities.insert(name, size);
                        }
                        // Pre-populate crate entries with priority and
                        // dependency-level `uses` from the config tree.
                        for task in &tasks {
                            let kind = classify_resolved_kind(&task.name, task.kind);
                            let c = build.crate_mut(&task.name, kind);
                            c.priority = Some(task.priority);
                            for dep in &task.depends_on {
                                c.uses.push(UsedResource {
                                    server_task: dep.clone(),
                                    resource: String::new(),
                                });
                            }
                        }
                    }
                }
                crate::bridge::Event::BuildResources {
                    build_id,
                    resources,
                    bundle,
                } => {
                    if let Some(build) = self.builds.get_mut(&build_id) {
                        build.resources = resources;
                        // Derive per-crate provides/uses from the IPC
                        // bundle, same logic the snapshot path uses.
                        apply_ipc_to_crates(&mut build.crates, &bundle);
                    }
                }
                crate::bridge::Event::BuildCrateState {
                    build_id,
                    name,
                    kind,
                    state,
                } => {
                    if let Some(build) = self.builds.get_mut(&build_id) {
                        let c = build.crate_mut(&name, kind);
                        c.state = state;
                    }
                }
                crate::bridge::Event::BuildCrateSized {
                    build_id,
                    name,
                    kind,
                    region,
                    size,
                } => {
                    if let Some(build) = self.builds.get_mut(&build_id) {
                        let c = build.crate_mut(&name, kind);
                        c.sizes.insert(region, size);
                        // Update total_size as sum of all regions.
                        c.total_size = Some(c.sizes.values().sum());
                    }
                }
                crate::bridge::Event::BuildCrateCargoLine {
                    build_id,
                    name,
                    kind,
                    line,
                } => {
                    if let Some(build) = self.builds.get_mut(&build_id) {
                        let c = build.crate_mut(&name, kind);
                        c.push_cargo_message(line);
                    }
                }
                crate::bridge::Event::BuildHostCrateState {
                    build_id,
                    name,
                    state,
                } => {
                    if let Some(build) = self.builds.get_mut(&build_id) {
                        let c = build.crate_mut(&name, CrateKind::HostCrate);
                        c.host_state = Some(state);
                        // Mirror to `state` for unified rendering.
                        c.state = match state {
                            HostCrateBuildState::Queued => CrateBuildState::Queued,
                            HostCrateBuildState::Building => CrateBuildState::Building,
                            HostCrateBuildState::Running => CrateBuildState::Measuring,
                            HostCrateBuildState::Done => CrateBuildState::Linked,
                            HostCrateBuildState::Failed => CrateBuildState::Failed,
                        };
                    }
                }
                crate::bridge::Event::BuildAllocation {
                    build_id,
                    allocation,
                } => {
                    if let Some(build) = self.builds.get_mut(&build_id) {
                        // Dedup by (owner, region) — later allocations
                        // (from deferred resolution) replace earlier ones.
                        if let Some(existing) = build.allocations.iter_mut().find(|a| {
                            a.owner == allocation.owner && a.region == allocation.region
                        }) {
                            *existing = allocation;
                        } else {
                            build.allocations.push(allocation);
                        }
                    }
                }
                crate::bridge::Event::BuildImage { build_id, image } => {
                    if let Some(build) = self.builds.get_mut(&build_id) {
                        build.image = image;
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
                                build.finished_at = Some(std::time::Instant::now());
                                build.phase = Some(PipelinePhase::Done);
                            }
                            // Don't auto-open a second Firmware tile for
                            // the archive — the existing Build tile
                            // already owns the view and has the full
                            // live state (host crates, cargo log,
                            // diagnostics). The firmware is registered
                            // in `self.firmware` so the user can open
                            // it manually from the sidebar for a
                            // "clean" loaded view later.
                        }
                        Err(error) => {
                            if let Some(build) = self.builds.get_mut(&build_id) {
                                build.log.push(format!("ERROR: {error}"));
                                build.status = BuildStatus::Failed { error };
                                build.finished_at = Some(std::time::Instant::now());
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
                crate::bridge::Event::FlashProgress { device_id, mut phase } => {
                    // When the stub is up (Done), use its IPC to write the
                    // target firmware: places.bin → `firmware` partition,
                    // then ftab.bin → `boot` partition.
                    if matches!(&phase, crate::bridge::FlashPhase::StubBooted) {
                        let firmware_id = match &self.flash_modal {
                            Some(FlashModalState::Flashing {
                                device_id: d,
                                firmware_id,
                                ..
                            }) if *d == device_id => Some(*firmware_id),
                            Some(FlashModalState::Picker {
                                firmware_id,
                                selected_device,
                            }) if *selected_device == Some(device_id) => Some(*firmware_id),
                            _ => None,
                        };

                        if let (Some(fw_id), Some(dev)) =
                            (firmware_id, self.devices.get(&device_id))
                        {
                            let archive_bytes = self.firmware.get(&fw_id).and_then(|fw| {
                                match &fw.path {
                                    Some(p) => match std::fs::read(p) {
                                        Ok(b) => Some(b),
                                        Err(e) => {
                                            eprintln!(
                                                "[flash] read archive {}: {e}",
                                                p.display()
                                            );
                                            None
                                        }
                                    },
                                    // Builtin = embedded stub; we'd never
                                    // flash it as a target.
                                    None => {
                                        eprintln!(
                                            "[flash] refusing to flash builtin firmware as target"
                                        );
                                        None
                                    }
                                }
                            });

                            match (archive_bytes, dev.ipc_registry.clone()) {
                                (Some(bytes), Some(registry)) => {
                                    let cmd_tx = self.cmd_tx.clone();
                                    self.tokio_handle.spawn(async move {
                                        run_flash(device_id, bytes, registry, cmd_tx).await;
                                    });
                                }
                                (None, _) => {
                                    let msg: String = "could not read firmware archive".into();
                                    phase = crate::bridge::FlashPhase::Failed {
                                        at_step: 3,
                                        message: msg.clone(),
                                    };
                                    let _ = self.cmd_tx.send(
                                        crate::bridge::Command::FlashFailed { device_id, message: msg },
                                    );
                                }
                                (_, None) => {
                                    let msg: String = "no IPC registry loaded; cannot flash".into();
                                    eprintln!("[flash] {msg}");
                                    phase = crate::bridge::FlashPhase::Failed {
                                        at_step: 3,
                                        message: msg.clone(),
                                    };
                                    let _ = self.cmd_tx.send(
                                        crate::bridge::Command::FlashFailed { device_id, message: msg },
                                    );
                                }
                            }
                        } else if firmware_id.is_none() {
                            eprintln!(
                                "[flash] no firmware_id in flash_modal for device {device_id:?}; skipping"
                            );
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
        tokio::sync::oneshot::Receiver<Result<crate::ipc::IpcCallResult, String>>,
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

    /// Fire a one-shot MoshiMoshi probe on the device's USART2. Used as a
    /// manual diagnostic to re-trigger the USART1 hello without restarting
    /// the app. No-op if the bridge can't find the device or its
    /// `SerialSender` capability — errors surface in bridge stderr.
    pub fn send_moshi_moshi(&self, device_id: DeviceId) {
        let _ = self
            .cmd_tx
            .send(crate::bridge::Command::SendMoshiMoshi { device_id });
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
            config: format!("apps/{config}.ncl"),
            board: format!("boards/{board}.ncl"),
            layout: format!("layouts/{layout}.ncl"),
        };

        self.builds.insert(
            build_id,
            BuildHandle {
                id: build_id,
                config: build_config.clone(),
                name: None,
                status: BuildStatus::Running,
                uuid: None,
                phase: None,
                crates: Vec::new(),
                allocations: Vec::new(),
                place_capacities: HashMap::new(),
                memories: Vec::new(),
                resources: Vec::new(),
                image: ImageProgress::None,
                log: vec![format!("Building {config} (board={board}, layout={layout})")],
                started_at: std::time::Instant::now(),
                finished_at: None,
            },
        );

        // Open the unified firmware panel against the live build.
        let pane = Pane::Firmware(build_id);
        let target = pane.clone();
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
        self.tree
            .make_active(|_, t| matches!(t, egui_tiles::Tile::Pane(p) if *p == target));

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

    /// Delete a build record and close any tiles rendering it. The
    /// firmware artifact (if any) stays on disk and in
    /// [`Self::firmware`] — users can still open the finished
    /// `.tfw` from the sidebar. Only clears the build's in-memory
    /// state (phase, cargo log, diagnostics, …).
    pub fn remove_build(&mut self, build_id: BuildId) {
        // If this build points at a firmware entry, remove the firmware
        // too — otherwise the sidebar still lists it and re-opening
        // just re-creates the snapshot.
        if let Some(build) = self.builds.get(&build_id) {
            if let BuildStatus::Succeeded {
                firmware_id: Some(fw_id),
                ..
            } = &build.status
            {
                if let Some(fw) = self.firmware.remove(fw_id) {
                    // Delete the .tfw file from the database.
                    if let Some(path) = &fw.path {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        }
        self.builds.remove(&build_id);
        let target = Pane::Firmware(build_id);
        let to_remove: Vec<egui_tiles::TileId> = self
            .tree
            .tiles
            .iter()
            .filter_map(|(tile_id, tile)| match tile {
                egui_tiles::Tile::Pane(p) if *p == target => Some(*tile_id),
                _ => None,
            })
            .collect();
        for tile_id in to_remove {
            self.tree.remove_recursively(tile_id);
        }
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

/// Write the target firmware (places.bin + ftab.bin) to the fob's flash
/// over IPC. Called after the RAMboot stub comes up — at that point the
/// stub is the only code running and has exclusive access to both
/// partitions.
///
/// Order is places → ftab deliberately: the ftab (sec_config) is the
/// BOOTROM's entry pointer, so writing it last makes the flash commit
/// atomic from the BOOTROM's perspective — either the new image is
/// fully staged or the old one is still being pointed at.
async fn run_flash(
    device_id: DeviceId,
    archive_bytes: Vec<u8>,
    registry: std::sync::Arc<ipc_runtime::Registry>,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<crate::bridge::Command>,
) {
    use std::io::{Cursor, Read};
    use zip::ZipArchive;

    let fail = |msg: String| {
        let _ = cmd_tx.send(crate::bridge::Command::FlashFailed {
            device_id,
            message: msg,
        });
    };

    let mut archive = match ZipArchive::new(Cursor::new(&archive_bytes)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[flash] archive open FAILED: {e}");
            fail(format!("archive open: {e}"));
            return;
        }
    };

    let read_entry = |archive: &mut ZipArchive<Cursor<&Vec<u8>>>, name: &str| -> Option<Vec<u8>> {
        let mut entry = match archive.by_name(name) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[flash] archive missing {name:?}: {e}");
                return None;
            }
        };
        let mut buf = Vec::new();
        if let Err(e) = entry.read_to_end(&mut buf) {
            eprintln!("[flash] read {name:?} FAILED: {e}");
            return None;
        }
        Some(buf)
    };

    let Some(places) = read_entry(&mut archive, "places.bin") else {
        fail("archive missing places.bin".into());
        return;
    };
    let Some(ftab) = read_entry(&mut archive, "ftab.bin") else {
        fail("archive missing ftab.bin".into());
        return;
    };
    drop(archive);

    if let Err(e) =
        flash_partition(device_id, &registry, &cmd_tx, "firmware", &places).await
    {
        eprintln!("[flash] firmware partition FAILED: {e}");
        fail(format!("firmware partition: {e}"));
        return;
    }
    if let Err(e) = flash_partition(device_id, &registry, &cmd_tx, "boot", &ftab).await {
        eprintln!("[flash] boot partition FAILED: {e}");
        fail(format!("boot partition: {e}"));
        return;
    }
    eprintln!("[flash] DONE ({} + {} bytes)", places.len(), ftab.len());
    let _ = cmd_tx.send(crate::bridge::Command::FlashComplete { device_id });
}

/// Acquire `partition_name`, erase enough to cover `data.len()`, then
/// program `data` in lease-sized chunks at offset 0. Uses separate
/// `erase` + `program` rather than `write` so the final chunk doesn't
/// need erase-size padding.
async fn flash_partition(
    device_id: DeviceId,
    registry: &std::sync::Arc<ipc_runtime::Registry>,
    cmd_tx: &tokio::sync::mpsc::UnboundedSender<crate::bridge::Command>,
    partition_name: &str,
    data: &[u8],
) -> Result<(), String> {
    use ipc_runtime::IpcValue;

    let name_bytes = partition_name.as_bytes();
    if name_bytes.len() > 16 {
        return Err(format!("partition name too long: {partition_name:?}"));
    }
    let mut padded = [0u8; 16];
    padded[..name_bytes.len()].copy_from_slice(name_bytes);
    let name_arr = IpcValue::Tuple(padded.iter().map(|b| IpcValue::U8(*b)).collect());
    let acquire_args = IpcValue::Struct(indexmap::indexmap! {
        "name".into() => name_arr,
    });

    eprintln!(
        "[flash] Partition::acquire({partition_name:?}) for {} bytes",
        data.len()
    );
    let partition = crate::ipc_handle::acquire(
        device_id,
        registry,
        cmd_tx,
        "Partition",
        "acquire",
        acquire_args,
    )
    .await
    .map_err(|e| format!("acquire: {e}"))?;

    let geometry = partition
        .call("geometry", IpcValue::Struct(indexmap::IndexMap::new()))
        .await
        .map_err(|e| format!("geometry: {e}"))?;
    let erase_size = geometry_field(&geometry, "erase_size")
        .ok_or_else(|| "missing erase_size in geometry".to_string())?
        as usize;
    if erase_size == 0 {
        return Err("erase_size=0 in geometry".into());
    }
    let erase_len = data.len().div_ceil(erase_size) * erase_size;

    eprintln!(
        "[flash]   erase(0, {erase_len}) (covers {} bytes, erase_size={erase_size})",
        data.len()
    );
    let _ = cmd_tx.send(crate::bridge::Command::FlashPhaseUpdate {
        device_id,
        phase: crate::bridge::FlashPhase::Erasing,
    });
    let erase_args = IpcValue::Struct(indexmap::indexmap! {
        "offset".into() => IpcValue::U32(0),
        "len".into()    => IpcValue::U32(erase_len as u32),
    });
    match partition.call("erase", erase_args).await {
        Ok(IpcValue::Ok(_)) => {}
        Ok(other) => return Err(format!("erase unexpected reply: {other:?}")),
        Err(e) => return Err(format!("erase: {e}")),
    }

    // Program in ~half-lease-pool chunks. The lease pool is 8 KiB
    // (rcard_usb_proto::LEASE_POOL_SIZE); 4 KiB leaves headroom for the
    // read-lease used by replies on the same pool.
    const CHUNK: usize = 4096;
    let data_total = data.len() as u32;
    let mut offset = 0usize;
    let _ = cmd_tx.send(crate::bridge::Command::FlashPhaseUpdate {
        device_id,
        phase: crate::bridge::FlashPhase::Programming {
            bytes_written: 0,
            bytes_total: data_total,
        },
    });
    while offset < data.len() {
        let end = (offset + CHUNK).min(data.len());
        let program_args = IpcValue::Struct(indexmap::indexmap! {
            "offset".into() => IpcValue::U32(offset as u32),
            "buf".into()    => IpcValue::Bytes(data[offset..end].to_vec()),
        });
        match partition.call("program", program_args).await {
            Ok(IpcValue::Ok(_)) => {}
            Ok(other) => {
                return Err(format!("program @ {offset:#x} unexpected reply: {other:?}"));
            }
            Err(e) => return Err(format!("program @ {offset:#x}: {e}")),
        }
        offset = end;
        let _ = cmd_tx.send(crate::bridge::Command::FlashPhaseUpdate {
            device_id,
            phase: crate::bridge::FlashPhase::Programming {
                bytes_written: offset as u32,
                bytes_total: data_total,
            },
        });
    }

    eprintln!(
        "[flash]   {partition_name:?} programmed ({} bytes in {} chunk(s))",
        data.len(),
        data.len().div_ceil(CHUNK)
    );

    // Read back and compare. Same chunk size — `Partition::read` fills
    // the writable lease the caller provides, returned via writeback.
    let mut offset = 0usize;
    let verify_start = std::time::Instant::now();
    let _ = cmd_tx.send(crate::bridge::Command::FlashPhaseUpdate {
        device_id,
        phase: crate::bridge::FlashPhase::Verifying {
            bytes_verified: 0,
            bytes_total: data_total,
        },
    });
    while offset < data.len() {
        let end = (offset + CHUNK).min(data.len());
        let chunk_len = end - offset;
        let read_args = IpcValue::Struct(indexmap::indexmap! {
            "offset".into() => IpcValue::U32(offset as u32),
            "buf".into()    => IpcValue::Bytes(vec![0u8; chunk_len]),
        });
        let chunk_start = std::time::Instant::now();
        let (reply, writeback) = partition
            .call_with_writeback("read", read_args)
            .await
            .map_err(|e| {
                let elapsed = verify_start.elapsed();
                format!(
                    "verify read @ {offset:#x} (chunk took {:.0}ms, total verify {:.0}ms): {e}",
                    chunk_start.elapsed().as_millis(),
                    elapsed.as_millis(),
                )
            })?;
        eprintln!(
            "[flash]   verify {offset:#x}..{end:#x} OK ({:.0}ms)",
            chunk_start.elapsed().as_millis(),
        );
        match reply {
            IpcValue::Ok(_) => {}
            other => return Err(format!("verify read @ {offset:#x} unexpected reply: {other:?}")),
        }
        if writeback.len() != chunk_len {
            return Err(format!(
                "verify @ {offset:#x}: short read ({} of {})",
                writeback.len(),
                chunk_len
            ));
        }
        if writeback != data[offset..end] {
            let want = &data[offset..end];
            let first_diff = writeback
                .iter()
                .zip(want.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(0);
            let mismatches = writeback
                .iter()
                .zip(want.iter())
                .filter(|(a, b)| a != b)
                .count();

            // Hex dump head + tail to see the corruption pattern.
            let dump = |label: &str, base: usize| {
                let hi = (base + 32).min(want.len());
                let w: Vec<String> = want[base..hi].iter().map(|b| format!("{b:02x}")).collect();
                let g: Vec<String> = writeback[base..hi]
                    .iter()
                    .map(|b| format!("{b:02x}")).collect();
                eprintln!(
                    "[flash]   {partition_name} {label} @{:#06x}",
                    offset + base
                );
                eprintln!("[flash]     want: {}", w.join(" "));
                eprintln!("[flash]      got: {}", g.join(" "));
            };
            dump("head", 0);
            if first_diff > 32 {
                dump("first-diff", first_diff.saturating_sub(8));
            }

            // Common corruption patterns worth checking explicitly.
            // Stride-2: every other byte got dropped.
            let stride2 = (0..writeback.len().min(want.len() / 2))
                .filter(|i| writeback[*i] == want[i * 2])
                .count();
            // Off-by-one shift.
            let shift1 = (0..writeback.len().saturating_sub(1).min(want.len()))
                .filter(|i| writeback[*i] == want.get(i + 1).copied().unwrap_or(0))
                .count();
            // All-zero / all-0xFF readback.
            let all_zero = writeback.iter().all(|&b| b == 0);
            let all_ff = writeback.iter().all(|&b| b == 0xFF);
            eprintln!(
                "[flash]   pattern: stride2={stride2}/{} shift1={shift1}/{} all_zero={all_zero} all_ff={all_ff}",
                want.len() / 2,
                want.len() - 1,
            );

            return Err(format!(
                "verify mismatch @ {:#x}: {mismatches}/{chunk_len} bytes differ, first at +{first_diff}",
                offset + first_diff,
            ));
        }
        offset = end;
        let _ = cmd_tx.send(crate::bridge::Command::FlashPhaseUpdate {
            device_id,
            phase: crate::bridge::FlashPhase::Verifying {
                bytes_verified: offset as u32,
                bytes_total: data_total,
            },
        });
    }

    eprintln!(
        "[flash]   {partition_name:?} verified ({} bytes round-tripped)",
        data.len()
    );
    drop(partition);
    Ok(())
}

/// Extract a named u64 field from a decoded `Geometry` struct.
fn geometry_field(v: &ipc_runtime::IpcValue, name: &str) -> Option<u64> {
    let ipc_runtime::IpcValue::Struct(fields) = v else {
        return None;
    };
    match fields.get(name)? {
        ipc_runtime::IpcValue::U32(n) => Some(*n as u64),
        ipc_runtime::IpcValue::U64(n) => Some(*n),
        _ => None,
    }
}
