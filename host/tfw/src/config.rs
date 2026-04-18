use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

// -- Top-level config --

/// Deserialized from the root .ncl file (e.g. fob.ncl, stub.ncl).
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct AppConfig {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    pub target: String,
    pub vector_table_size: u32,

    /// Physical memory devices from the board.
    pub memory: BTreeMap<String, MemoryDevice>,

    /// Named places — the layout's mapping of intent to hardware.
    pub places: BTreeMap<String, Place>,

    #[serde(default)]
    pub peripheral_map: BTreeMap<String, PeripheralDef>,
    #[serde(default)]
    pub pins: BTreeMap<String, PinDef>,
    #[serde(default)]
    pub pin_assignments: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    pub notifications: BTreeMap<String, NotificationGroup>,
    #[serde(default)]
    pub filesystems: BTreeMap<String, FilesystemConfig>,
    /// App-level ACL exception: tasks listed here are permitted to send to
    /// any other task, independent of `depends_on` edges. Used for
    /// privileged dispatchers whose call set cannot be expressed in the
    /// dependency graph.
    #[serde(default)]
    pub trusted_senders: Vec<TaskConfig>,

    /// Boot config — where to write the ftab and firmware in the flash image.
    #[serde(default)]
    pub boot: Option<BootConfig>,

    /// Bootloader config — optional, comes from the board.
    #[serde(default)]
    pub bootloader: Option<BootloaderConfig>,

    /// Kernel config — separate from tasks, comes from the board.
    pub kernel: KernelConfig,
    /// Root tasks — each carries its full dependency tree.
    pub entries: Vec<TaskConfig>,
}

// -- Memory devices --

/// A physical memory device. Has a size, optional flash geometry, and
/// zero or more CPU mappings.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct MemoryDevice {
    pub size: u64,
    /// Full flash/storage geometry (erase/program/read granularities).
    /// Flows into the storage sysmodule as build-time constants.
    #[serde(default)]
    pub geometry: Option<DeviceGeometry>,
    #[serde(default)]
    pub mappings: Vec<Mapping>,
    /// Offset within a parent device (set by lib.partition).
    #[serde(default)]
    pub offset: Option<u64>,
}

/// Read/program/erase granularities of a flash device. Static hardware
/// fact — baked at build time into the storage sysmodule.
#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize)]
pub struct DeviceGeometry {
    pub erase_size: u64,
    pub program_size: u64,
    pub read_size: u64,
}

/// A CPU-visible mapping of a memory device.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct Mapping {
    pub address: u64,
    pub size: u64,
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub write: bool,
    #[serde(default)]
    pub execute: bool,
}

// -- Places --

/// A named place from the layout. Carries the underlying device info
/// (inherited from the memory device, possibly with offset from partitioning).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct Place {
    pub size: u64,
    /// Inherited from the backing memory device.
    #[serde(default)]
    pub geometry: Option<DeviceGeometry>,
    #[serde(default)]
    pub mappings: Vec<Mapping>,
    #[serde(default)]
    pub offset: Option<u64>,
    /// If true, this place is not CPU-mapped even if the device has mappings.
    /// Access goes through a driver (ACL only, no linker section).
    #[serde(default)]
    pub unmapped: bool,
    /// Place name from `config.places`. Not set by Nickel — populated by
    /// [`stamp_place_names`] after deserialization so that downstream code
    /// (layout solver, event emitters) can refer to the place by name.
    #[serde(default)]
    pub name: Option<String>,
}

// -- Boot config --

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct BootConfig {
    /// Place where the ftab (partition table) is written. The ftab's
    /// boot target is derived from the bootloader's code region
    /// placement in the layout.
    pub ftab: Place,
    /// Place where `places.bin` (the flashed firmware image) lives. The
    /// bootloader needs its flash address and size to locate the image
    /// at boot.
    pub image: Place,
}

// -- Bootloader --

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct BootloaderConfig {
    pub crate_info: CrateInfo,
    #[serde(default)]
    pub regions: BTreeMap<String, RegionRequest>,
}

// -- Kernel --

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct KernelConfig {
    pub crate_info: CrateInfo,
    #[serde(default)]
    pub regions: BTreeMap<String, RegionRequest>,
}

// -- Tasks (recursive) --

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct TaskConfig {
    pub crate_info: CrateInfo,
    pub priority: u32,
    #[serde(default)]
    pub regions: BTreeMap<String, RegionRequest>,
    #[serde(default)]
    pub supervisor: bool,
    #[serde(default)]
    pub depends_on: Vec<TaskConfig>,
    #[serde(default)]
    pub peers: Vec<String>,
    #[serde(default)]
    pub uses_peripherals: Vec<String>,
    #[serde(default)]
    pub uses_partitions: Vec<String>,
    #[serde(default)]
    pub pushes_notifications: Vec<String>,
    #[serde(default)]
    pub uses_notifications: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct CrateInfo {
    pub package: CratePackage,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct CratePackage {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// A region request: a place with optional size/alignment constraints.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct RegionRequest {
    /// The resolved place from the layout.
    pub place: Place,
    /// Size in bytes for this region's allocation within the place.
    /// None = sized by linker (e.g. code, data sections).
    #[serde(default)]
    pub size: Option<u64>,
    /// Alignment in bytes.
    #[serde(default)]
    pub align: Option<u64>,
    /// If true, this region is shared with other tasks that declare the same name.
    #[serde(default)]
    pub shared: bool,
}

// -- Peripherals --

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct PeripheralDef {
    pub base: u64,
    pub size: u64,
    #[serde(default)]
    pub irqs: BTreeMap<String, u32>,
    #[serde(default)]
    pub renode: Option<RenodeModel>,
}

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct RenodeModel {
    pub model: String,
    #[serde(default)]
    pub properties: BTreeMap<String, u64>,
}

// -- Pins --

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct PinDef {
    pub default_pull: String,
    #[serde(default)]
    pub supports: Vec<PinCapability>,
}

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct PinCapability {
    pub kind: String,
    #[serde(default)]
    pub instance: Option<u32>,
    #[serde(default)]
    pub mode: Option<String>,
    pub signals: Vec<String>,
}

// -- Notifications --

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct NotificationGroup {
    pub min_priority: u32,
    pub max_priority: u32,
}

// -- Filesystems --

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct FilesystemConfig {
    pub mounts: Vec<FsMount>,
}

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct FsMount {
    pub name: String,
    pub source: String,
}

// -- Discovery + evaluation (unchanged) --

pub fn discover_tasks(firmware_dir: &Path) -> BTreeMap<String, PathBuf> {
    let mut tasks = BTreeMap::new();

    for pattern in ["sysmodule/*/task.ncl", "tasks/*/task.ncl"] {
        let full = firmware_dir.join(pattern).display().to_string().replace('\\', "/");
        for entry in glob::glob(&full).into_iter().flatten().flatten() {
            let rel = entry
                .strip_prefix(firmware_dir)
                .unwrap_or(&entry);
            let name = task_name_from_path(rel);
            tasks.insert(name, entry);
        }
    }

    tasks
}

fn task_name_from_path(rel: &Path) -> String {
    let parts: Vec<&str> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();

    match parts.as_slice() {
        ["sysmodule", name, "task.ncl"] => format!("sysmodule_{name}"),
        ["tasks", name, "task.ncl"] => name.to_string(),
        _ => {
            let stem = rel.parent().unwrap_or(rel);
            stem.to_string_lossy().replace(['/', '\\'], "_")
        }
    }
}

fn build_shim(
    firmware_dir: &Path,
    root_ncl: &str,
    board_ncl: &str,
    layout_ncl: &str,
    tasks: &BTreeMap<String, PathBuf>,
) -> String {
    let root = firmware_dir.join(root_ncl).display().to_string().replace('\\', "/");
    let board = firmware_dir.join(board_ncl).display().to_string().replace('\\', "/");
    let layout = firmware_dir.join(layout_ncl).display().to_string().replace('\\', "/");

    let task_imports: Vec<String> = tasks
        .iter()
        .map(|(name, path)| {
            let p = path.display().to_string().replace('\\', "/");
            format!("    {name} = import \"{p}\"")
        })
        .collect();

    format!(
        r#"let _board = import "{board}" in
  let _layout = (import "{layout}") _board.memory in
  let task_fns = {{
{task_imports}
  }} in
  let rec ctx = {{
    tasks = std.record.map (fun _name f => f ctx) task_fns,
    places = _layout,
  }} in
  (import "{root}") {{ board = _board, tasks = ctx.tasks, places = _layout }}"#,
        task_imports = task_imports.join(",\n"),
    )
}

pub fn load(
    firmware_dir: &Path,
    root_ncl: &str,
    board_ncl: &str,
    layout_ncl: &str,
) -> Result<AppConfig, ConfigError> {
    let tasks = discover_tasks(firmware_dir);
    let shim = build_shim(firmware_dir, root_ncl, board_ncl, layout_ncl, &tasks);

    let mut config: AppConfig =
        nickel_lang_core::deserialize::from_str(&shim)
            .map_err(|e| ConfigError::Eval(ConfigEvalError::from(e)))?;

    stamp_place_names(&mut config);

    Ok(config)
}

/// Walk all `Place` structs in the config and stamp each with the name
/// of the matching entry from `config.places`. Matching is by identity:
/// same size, same offset, same first mapping address.
fn stamp_place_names(config: &mut AppConfig) {
    // Build lookup: (size, offset, first_mapping_addr) → place name.
    let lookup: BTreeMap<(u64, u64, u64), String> = config
        .places
        .iter()
        .filter_map(|(name, place)| {
            let addr = place.mappings.first().map(|m| m.address).unwrap_or(0);
            Some(((place.size, place.offset.unwrap_or(0), addr), name.clone()))
        })
        .collect();

    fn stamp(place: &mut Place, lookup: &BTreeMap<(u64, u64, u64), String>) {
        if place.name.is_none() {
            let addr = place.mappings.first().map(|m| m.address).unwrap_or(0);
            let key = (place.size, place.offset.unwrap_or(0), addr);
            if let Some(name) = lookup.get(&key) {
                place.name = Some(name.clone());
            }
        }
    }

    fn stamp_regions(
        regions: &mut BTreeMap<String, RegionRequest>,
        lookup: &BTreeMap<(u64, u64, u64), String>,
    ) {
        for req in regions.values_mut() {
            stamp(&mut req.place, lookup);
        }
    }

    // Stamp top-level places.
    for (name, place) in &mut config.places {
        place.name = Some(name.clone());
    }

    // Stamp kernel regions.
    stamp_regions(&mut config.kernel.regions, &lookup);

    // Stamp bootloader regions.
    if let Some(bl) = &mut config.bootloader {
        stamp_regions(&mut bl.regions, &lookup);
    }

    // Stamp task regions (recursive).
    fn stamp_tasks(
        entries: &mut [TaskConfig],
        lookup: &BTreeMap<(u64, u64, u64), String>,
    ) {
        for task in entries {
            stamp_regions(&mut task.regions, lookup);
            stamp_tasks(&mut task.depends_on, lookup);
        }
    }
    stamp_tasks(&mut config.entries, &lookup);
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config evaluation failed:\n{0}")]
    Eval(ConfigEvalError),
}

/// Wrapper around Nickel's `EvalOrDeserError` that is `Send` via `Mutex`.
/// The mutex is uncontended in practice — errors are only read once for display.
#[derive(Debug)]
pub struct ConfigEvalError(std::sync::Mutex<nickel_lang_core::deserialize::EvalOrDeserError>);

impl From<nickel_lang_core::deserialize::EvalOrDeserError> for ConfigEvalError {
    fn from(e: nickel_lang_core::deserialize::EvalOrDeserError) -> Self {
        Self(std::sync::Mutex::new(e))
    }
}

impl std::fmt::Display for ConfigEvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.0.lock().unwrap();
        write!(f, "{inner}")
    }
}

// SAFETY: The inner `EvalOrDeserError` is only accessed through the `Mutex`,
// which provides synchronization. The `Rc` inside cannot be cloned out.
unsafe impl Send for ConfigEvalError {}
unsafe impl Sync for ConfigEvalError {}
