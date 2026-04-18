use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::config::AppConfig;
use crate::layout;

/// The master config JSON that `firmware/modules/generated/build.rs` reads.
#[derive(Debug, Serialize)]
pub struct GeneratedConfig {
    /// Build UUID.
    pub build_id: String,
    /// App version from the config.
    pub version: Option<String>,

    /// Ordered task list (crate names). Index = task slot index.
    pub tasks: Vec<String>,
    /// Reverse map: crate name → slot index.
    pub task_indices: BTreeMap<String, usize>,

    /// Notification groups, sorted alphabetically.
    pub notifications: Vec<NotificationGroupConfig>,

    /// Which tasks push to which notification groups (task index → group IDs).
    pub notification_pushers: BTreeMap<usize, Vec<usize>>,
    /// Which tasks subscribe to which notification groups (task index → group IDs).
    pub notification_subscribers: BTreeMap<usize, Vec<usize>>,

    /// Partition config for storage.
    pub partitions: Vec<PartitionEntry>,
    /// Partition ACLs: partition name → list of allowed task indices.
    pub partition_acl: BTreeMap<String, Vec<usize>>,
    /// Static geometry per memory device, keyed by device name (e.g. `"mpi2"`).
    /// Surfaces the NCL `memory.<device>.geometry` field for firmware consumers.
    pub device_geometry: BTreeMap<String, DeviceGeometryEntry>,

    /// Filesystem mount mappings.
    pub filesystems: Vec<FilesystemEntry>,

    /// Pin assignments from the board config.
    pub pin_assignments: BTreeMap<String, BTreeMap<String, String>>,

    /// IPC ACL: server crate name → list of allowed caller task indices.
    pub ipc_acl: BTreeMap<String, Vec<usize>>,

    /// Peer references: peer name → task index (if present in the build).
    /// Collected from all tasks' `peers` fields. A peer that isn't in the
    /// build gets `null` so the generated code can use `Option<TaskId>`.
    pub peers: BTreeMap<String, Option<usize>>,

    /// Per-task IRQ name → notification bit mapping.
    /// Outer key: task crate name. Inner key: `{peripheral}_{irq_name}` from
    /// the peripheral_map. Value: the notification bit the kernel posts when
    /// the IRQ fires (matches what tfw's compile.rs already wires into the
    /// kernel's IRQ-to-task table).
    pub task_irqs: BTreeMap<String, BTreeMap<String, u32>>,
}

#[derive(Debug, Serialize)]
pub struct NotificationGroupConfig {
    pub name: String,
    pub group_id: usize,
    pub min_priority: u32,
    pub max_priority: u32,
}

#[derive(Debug, Serialize)]
pub struct PartitionEntry {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

#[derive(Debug, Serialize)]
pub struct FilesystemEntry {
    pub filesystem: String,
    pub mount_name: String,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct DeviceGeometryEntry {
    pub erase_size: u64,
    pub program_size: u64,
    pub read_size: u64,
}

/// Build the master config from the resolved AppConfig.
pub fn build_config(config: &AppConfig, build_id: &str) -> GeneratedConfig {
    let all_tasks = layout::collect_tasks(config);

    // Canonical ordering: (workgroup, dep_depth, name).
    let task_names = layout::ordered_task_names(&all_tasks);
    let tasks: Vec<String> = task_names.iter().map(|name| name.to_string()).collect();
    let task_indices: BTreeMap<String, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, name)| (name.clone(), i))
        .collect();

    // Notification groups — sorted alphabetically, assigned IDs.
    let mut group_names: Vec<&String> = config.notifications.keys().collect();
    group_names.sort();

    let notifications: Vec<NotificationGroupConfig> = group_names
        .iter()
        .enumerate()
        .map(|(id, name)| {
            let group = &config.notifications[*name];
            NotificationGroupConfig {
                name: name.to_string(),
                group_id: id,
                min_priority: group.min_priority,
                max_priority: group.max_priority,
            }
        })
        .collect();

    let group_id_by_name: BTreeMap<&str, usize> = group_names
        .iter()
        .enumerate()
        .map(|(id, name)| (name.as_str(), id))
        .collect();

    // Notification pushers and subscribers.
    let mut notification_pushers: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut notification_subscribers: BTreeMap<usize, Vec<usize>> = BTreeMap::new();

    for (crate_name, task) in &all_tasks {
        if let Some(&task_idx) = task_indices.get(*crate_name) {
            for group_name in &task.pushes_notifications {
                if let Some(&gid) = group_id_by_name.get(group_name.as_str()) {
                    notification_pushers.entry(task_idx).or_default().push(gid);
                }
            }
            for group_name in &task.uses_notifications {
                if let Some(&gid) = group_id_by_name.get(group_name.as_str()) {
                    notification_subscribers
                        .entry(task_idx)
                        .or_default()
                        .push(gid);
                }
            }
        }
    }

    // Partitions — places that have an offset (from lib.partition).
    let mut partitions = Vec::new();
    for (place_name, place) in &config.places {
        if let Some(offset) = place.offset {
            partitions.push(PartitionEntry {
                name: place_name.clone(),
                offset,
                size: place.size,
            });
        }
    }

    // Place ACLs — derived from task regions and uses_partitions.
    let mut partition_acl: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (crate_name, task) in &all_tasks {
        if let Some(&task_idx) = task_indices.get(*crate_name) {
            for region_name in task.regions.keys() {
                partition_acl
                    .entry(region_name.clone())
                    .or_default()
                    .push(task_idx);
            }
            for part_name in &task.uses_partitions {
                partition_acl
                    .entry(part_name.clone())
                    .or_default()
                    .push(task_idx);
            }
        }
    }
    for list in partition_acl.values_mut() {
        list.sort_unstable();
        list.dedup();
    }

    // Filesystem mounts.
    let mut filesystems = Vec::new();
    for (fs_name, fs_config) in &config.filesystems {
        for mount in &fs_config.mounts {
            filesystems.push(FilesystemEntry {
                filesystem: fs_name.clone(),
                mount_name: mount.name.clone(),
                source: mount.source.clone(),
            });
        }
    }

    // IPC ACL — derived from the dependency graph.
    // If task A depends on task B, A is allowed to call B. Strictly one-way:
    // `peers` does NOT grant ACL (only TaskId visibility), to avoid
    // accidentally handing out reverse-direction call rights.
    let mut ipc_acl: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (client_name, task) in &all_tasks {
        if let Some(&client_idx) = task_indices.get(*client_name) {
            for dep in &task.depends_on {
                let server_name = &dep.crate_info.package.name;
                ipc_acl
                    .entry(server_name.clone())
                    .or_default()
                    .push(client_idx);
            }
        }
    }

    // App-level trusted_senders: tasks listed in the app's .ncl file are
    // permitted to send to any other task, independent of `depends_on`
    // edges. Each trusted sender's index is added to every task's allowlist.
    let trusted_indices: Vec<usize> = config
        .trusted_senders
        .iter()
        .filter_map(|t| task_indices.get(&t.crate_info.package.name).copied())
        .collect();
    if !trusted_indices.is_empty() {
        for receiver_name in &tasks {
            let entry = ipc_acl.entry(receiver_name.clone()).or_default();
            for &idx in &trusted_indices {
                entry.push(idx);
            }
        }
    }

    // Deduplicate
    for list in ipc_acl.values_mut() {
        list.sort_unstable();
        list.dedup();
    }

    // Collect all peer references and resolve to task indices (if present).
    let mut peers: BTreeMap<String, Option<usize>> = BTreeMap::new();
    for (_crate_name, task) in &all_tasks {
        for peer_name in &task.peers {
            let idx = task_indices.get(peer_name.as_str()).copied();
            peers.insert(peer_name.clone(), idx);
        }
    }

    // Per-task IRQ map. Mirrors tfw/compile.rs's IRQ-to-task wiring: every
    // peripheral a task lists in `uses_peripherals` contributes its
    // peripheral_map IRQs, with the same `1 << (irq_num % 32)` bit assignment.
    // Names are qualified `{peripheral}_{irq_name}` to avoid collisions when
    // a task uses multiple peripherals that happen to share inner IRQ names.
    let mut task_irqs: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    for (crate_name, task) in &all_tasks {
        let mut entries: BTreeMap<String, u32> = BTreeMap::new();
        for periph_name in &task.uses_peripherals {
            if let Some(periph) = config.peripheral_map.get(periph_name) {
                for (irq_name, &irq_num) in &periph.irqs {
                    let qualified = format!("{periph_name}_{irq_name}");
                    entries.insert(qualified, 1u32 << (irq_num % 32));
                }
            }
        }
        if !entries.is_empty() {
            task_irqs.insert(crate_name.to_string(), entries);
        }
    }

    // Per-device geometry. Only devices that declare a geometry block
    // surface here — the NCL is the source of truth, and sysmodule_storage
    // bakes these as build-time constants.
    // `memory` lives on `config.kernel` indirectly — we grab it via
    // the chip-level MemoryDevice map on the top-level AppConfig.
    let mut device_geometry: BTreeMap<String, DeviceGeometryEntry> = BTreeMap::new();
    for (dev_name, dev) in memory_devices(config) {
        if let Some(g) = dev.geometry {
            device_geometry.insert(
                dev_name.clone(),
                DeviceGeometryEntry {
                    erase_size: g.erase_size,
                    program_size: g.program_size,
                    read_size: g.read_size,
                },
            );
        }
    }

    GeneratedConfig {
        build_id: build_id.to_string(),
        version: config.version.clone(),
        tasks,
        task_indices,
        notifications,
        notification_pushers,
        notification_subscribers,
        partitions,
        partition_acl,
        device_geometry,
        filesystems,
        pin_assignments: config.pin_assignments.clone(),
        ipc_acl,
        peers,
        task_irqs,
    }
}

/// Iterate over memory devices declared in the app config. The location
/// depends on how `memory` is threaded through — for this project it's
/// surfaced via the `AppConfig::memory` map.
fn memory_devices(config: &AppConfig) -> impl Iterator<Item = (&String, &crate::config::MemoryDevice)> {
    config.memory.iter()
}

/// Write the master config JSON to the given path.
pub fn emit(config: &AppConfig, build_id: &str, out_path: &Path) -> Result<(), CodegenError> {
    let generated = build_config(config, build_id);

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(CodegenError::Io)?;
    }

    let json = serde_json::to_string_pretty(&generated).map_err(CodegenError::Json)?;
    std::fs::write(out_path, json).map_err(CodegenError::Io)?;

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CodegenError {
    #[error("codegen IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("codegen JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
