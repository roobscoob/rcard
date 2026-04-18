#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    build_id: Option<String>,
    #[serde(default)]
    version: Option<String>,
    tasks: Vec<String>,
    task_indices: std::collections::BTreeMap<String, usize>,
    notifications: Vec<NotificationGroup>,
    notification_pushers: std::collections::BTreeMap<usize, Vec<usize>>,
    notification_subscribers: std::collections::BTreeMap<usize, Vec<usize>>,
    #[serde(default)]
    ipc_acl: std::collections::BTreeMap<String, Vec<usize>>,
    #[serde(default)]
    partition_acl: std::collections::BTreeMap<String, Vec<usize>>,
    #[serde(default)]
    partitions: Vec<PartitionEntry>,
    #[serde(default)]
    filesystems: Vec<FilesystemEntry>,
    #[serde(default)]
    #[allow(dead_code)]
    pin_assignments: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    peers: std::collections::BTreeMap<String, Option<usize>>,
    #[serde(default)]
    task_irqs: std::collections::BTreeMap<String, std::collections::BTreeMap<String, u32>>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct PartitionEntry {
    name: String,
    offset: u64,
    size: u64,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct FilesystemEntry {
    filesystem: String,
    mount_name: String,
    source: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct NotificationGroup {
    name: String,
    group_id: usize,
    min_priority: u32,
    max_priority: u32,
}

fn main() {
    let json_path = if let Ok(path) = std::env::var("TFW_CONFIG_JSON") {
        PathBuf::from(path)
    } else {
        let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let project_root = manifest_dir.parent().unwrap().parent().unwrap();
        project_root.join(".work").join("config.json")
    };

    println!("cargo::rerun-if-env-changed=TFW_CONFIG_JSON");
    println!("cargo::rerun-if-changed={}", json_path.display());

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());


    if !json_path.exists() {
        // Generate stubs for cargo check / rust-analyzer
        write_task_stubs(&out_dir);
        write_notification_stubs(&out_dir);
        write_slot_stubs(&out_dir);
        write_peer_stubs(&out_dir);
        write_acl_stubs(&out_dir);
        write_irq_stubs(&out_dir);
        write_partitions_stubs(&out_dir);
        write_build_info_stubs(&out_dir);
        return;
    }

    let data: Config =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();

    write_tasks(&out_dir, &data);
    write_notifications(&out_dir, &data);
    write_slots(&out_dir, &data);
    write_peers(&out_dir, &data);
    write_acl(&out_dir, &data);
    write_irqs(&out_dir, &data);
    write_partitions(&out_dir, &data);
    write_build_info(&out_dir, &data);
}

fn write_tasks(out_dir: &PathBuf, config: &Config) {
    let path = out_dir.join("tasks.rs");
    let mut f = std::fs::File::create(&path).unwrap();

    let count = config.tasks.len();
    writeln!(f, "pub const TASK_COUNT: usize = {count};").unwrap();
    writeln!(f).unwrap();

    writeln!(f, "pub const TASK_NAMES: [&str; {count}] = [").unwrap();
    for name in &config.tasks {
        writeln!(f, "    \"{name}\",").unwrap();
    }
    writeln!(f, "];").unwrap();
    writeln!(f).unwrap();

    // Per-task index constants — used by `ipc::server!` to emit
    // task_id into `.ipc_meta` server records so the host can route
    // IPC calls to the correct task.
    for (i, name) in config.tasks.iter().enumerate() {
        let screaming = name.replace('-', "_").to_uppercase();
        writeln!(f, "#[allow(dead_code)]").unwrap();
        writeln!(f, "pub const TASK_ID_{screaming}: u16 = {i};").unwrap();
    }
}

fn write_notifications(out_dir: &PathBuf, config: &Config) {
    let path = out_dir.join("notifications.rs");
    let mut f = std::fs::File::create(&path).unwrap();

    let count = config.notifications.len();

    // NotificationGroup struct + GROUPS array
    writeln!(f, "#[allow(dead_code)]").unwrap();
    writeln!(f, "pub struct NotificationGroup {{").unwrap();
    writeln!(f, "    pub name: &'static str,").unwrap();
    writeln!(f, "    pub priority_range: core::ops::RangeInclusive<u8>,").unwrap();
    writeln!(f, "}}").unwrap();
    writeln!(f).unwrap();

    writeln!(f, "#[allow(dead_code)]").unwrap();
    writeln!(f, "pub const GROUPS: &[NotificationGroup; {count}] = &[").unwrap();
    for group in &config.notifications {
        writeln!(
            f,
            "    NotificationGroup {{ name: \"{}\", priority_range: {}..={} }},",
            group.name, group.min_priority, group.max_priority
        )
        .unwrap();
    }
    writeln!(f, "];").unwrap();
    writeln!(f).unwrap();

    // GROUP_ID constants
    for group in &config.notifications {
        let screaming = group.name.to_uppercase().replace('-', "_");
        writeln!(f, "#[allow(dead_code)]").unwrap();
        writeln!(
            f,
            "pub const GROUP_ID_{screaming}: u16 = {};",
            group.group_id
        )
        .unwrap();
    }
    writeln!(f).unwrap();

    writeln!(f, "pub const GROUP_COUNT: usize = {count};").unwrap();
    writeln!(f).unwrap();

    // is_sender_allowed
    writeln!(
        f,
        "pub fn is_sender_allowed(group_id: u16, sender: u16) -> bool {{"
    )
    .unwrap();
    writeln!(f, "    match group_id {{").unwrap();
    for group in &config.notifications {
        let gid = group.group_id;
        // Find all tasks that push to this group
        let pushers: Vec<usize> = config
            .notification_pushers
            .iter()
            .filter(|(_, groups)| groups.contains(&gid))
            .map(|(task_idx, _)| *task_idx)
            .collect();
        if pushers.is_empty() {
            writeln!(f, "        {gid} => false,").unwrap();
        } else {
            let checks: Vec<String> = pushers.iter().map(|i| format!("sender == {i}")).collect();
            writeln!(f, "        {gid} => {},", checks.join(" || ")).unwrap();
        }
    }
    writeln!(f, "        _ => false,").unwrap();
    writeln!(f, "    }}").unwrap();
    writeln!(f, "}}").unwrap();
    writeln!(f).unwrap();

    // group_subscribers
    writeln!(
        f,
        "pub fn group_subscribers(group_id: u16) -> &'static [u16] {{"
    )
    .unwrap();
    writeln!(f, "    match group_id {{").unwrap();
    for group in &config.notifications {
        let gid = group.group_id;
        let subs: Vec<usize> = config
            .notification_subscribers
            .iter()
            .filter(|(_, groups)| groups.contains(&gid))
            .map(|(task_idx, _)| *task_idx)
            .collect();
        let vals: Vec<String> = subs.iter().map(|i| format!("{i}")).collect();
        writeln!(f, "        {gid} => &[{}],", vals.join(", ")).unwrap();
    }
    writeln!(f, "        _ => &[],").unwrap();
    writeln!(f, "    }}").unwrap();
    writeln!(f, "}}").unwrap();
    writeln!(f).unwrap();

    // is_subscriber
    writeln!(
        f,
        "pub fn is_subscriber(group_id: u16, task: u16) -> bool {{"
    )
    .unwrap();
    writeln!(f, "    match group_id {{").unwrap();
    for group in &config.notifications {
        let gid = group.group_id;
        let subs: Vec<usize> = config
            .notification_subscribers
            .iter()
            .filter(|(_, groups)| groups.contains(&gid))
            .map(|(task_idx, _)| *task_idx)
            .collect();
        if subs.is_empty() {
            writeln!(f, "        {gid} => false,").unwrap();
        } else {
            let checks: Vec<String> = subs.iter().map(|i| format!("task == {i}")).collect();
            writeln!(f, "        {gid} => {},", checks.join(" || ")).unwrap();
        }
    }
    writeln!(f, "        _ => false,").unwrap();
    writeln!(f, "    }}").unwrap();
    writeln!(f, "}}").unwrap();
    writeln!(f).unwrap();

    // SUBSCRIBER_TASKS: all unique task indices that subscribe to any group
    let mut all_subs: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for (task_idx, _) in &config.notification_subscribers {
        all_subs.insert(*task_idx);
    }
    let sub_vals: Vec<String> = all_subs.iter().map(|i| format!("{i}")).collect();
    writeln!(f, "#[allow(dead_code)]").unwrap();
    writeln!(f, "pub const SUBSCRIBER_TASKS: &[u16] = &[{}];", sub_vals.join(", ")).unwrap();
    writeln!(f).unwrap();

    // Per-group subscriber constants: <NAME>_SUBSCRIBERS
    for group in &config.notifications {
        let gid = group.group_id;
        let subs: Vec<usize> = config
            .notification_subscribers
            .iter()
            .filter(|(_, groups)| groups.contains(&gid))
            .map(|(task_idx, _)| *task_idx)
            .collect();
        let screaming = group.name.to_uppercase().replace('-', "_");
        let vals: Vec<String> = subs.iter().map(|i| format!("{i}")).collect();
        writeln!(f, "#[allow(dead_code)]").unwrap();
        writeln!(
            f,
            "pub const {screaming}_SUBSCRIBERS: &[u16] = &[{}];",
            vals.join(", ")
        )
        .unwrap();
    }
}

fn write_task_stubs(out_dir: &PathBuf) {
    let path = out_dir.join("tasks.rs");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "pub const TASK_COUNT: usize = 0;").unwrap();
    writeln!(f, "pub const TASK_NAMES: [&str; 0] = [];").unwrap();
}

fn write_notification_stubs(out_dir: &PathBuf) {
    let path = out_dir.join("notifications.rs");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "pub const GROUP_COUNT: usize = 0;").unwrap();
    writeln!(
        f,
        "pub fn is_sender_allowed(_group_id: u16, _sender: u16) -> bool {{ false }}"
    )
    .unwrap();
    writeln!(
        f,
        "pub fn group_subscribers(_group_id: u16) -> &'static [u16] {{ &[] }}"
    )
    .unwrap();
    writeln!(
        f,
        "pub fn is_subscriber(_group_id: u16, _task: u16) -> bool {{ false }}"
    )
    .unwrap();
}

fn write_slots(out_dir: &PathBuf, config: &Config) {
    let path = out_dir.join("slots.rs");
    let mut f = std::fs::File::create(&path).unwrap();

    // Generate the Slots struct
    writeln!(f, "pub struct Slots {{").unwrap();
    for name in &config.tasks {
        writeln!(f, "    pub {name}: userlib::TaskId,").unwrap();
    }
    writeln!(f, "}}").unwrap();
    writeln!(f).unwrap();

    // Generate the SLOTS constant
    writeln!(f, "pub const SLOTS: Slots = Slots {{").unwrap();
    for (name, idx) in config.task_indices.iter() {
        writeln!(f, "    {name}: userlib::TaskId::gen0({idx}),").unwrap();
    }
    writeln!(f, "}};").unwrap();
}

fn write_slot_stubs(out_dir: &PathBuf) {
    let path = out_dir.join("slots.rs");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "pub struct Slots {{}}").unwrap();
    writeln!(f, "pub const SLOTS: Slots = Slots {{}};").unwrap();
}

fn write_peers(out_dir: &PathBuf, config: &Config) {
    let path = out_dir.join("peers.rs");
    let mut f = std::fs::File::create(&path).unwrap();

    writeln!(f, "pub struct Peers {{").unwrap();
    for name in config.peers.keys() {
        writeln!(f, "    pub {name}: Option<userlib::TaskId>,").unwrap();
    }
    writeln!(f, "}}").unwrap();
    writeln!(f).unwrap();

    writeln!(f, "pub const PEERS: Peers = Peers {{").unwrap();
    for (name, idx) in &config.peers {
        match idx {
            Some(i) => writeln!(f, "    {name}: Some(userlib::TaskId::gen0({i})),").unwrap(),
            None => writeln!(f, "    {name}: None,").unwrap(),
        }
    }
    writeln!(f, "}};").unwrap();
}

fn write_peer_stubs(out_dir: &PathBuf) {
    let path = out_dir.join("peers.rs");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "pub struct Peers {{}}").unwrap();
    writeln!(f, "pub const PEERS: Peers = Peers {{}};").unwrap();
}

fn write_acl(out_dir: &PathBuf, config: &Config) {
    let path = out_dir.join("acl.rs");
    let mut f = std::fs::File::create(&path).unwrap();

    writeln!(f, "#[macro_export]").unwrap();
    writeln!(f, "macro_rules! acl_check {{").unwrap();
    for task_name in &config.tasks {
        let arm_name = task_name.replace('-', "_");
        let allowed = config.ipc_acl.get(task_name);
        match allowed {
            Some(list) if !list.is_empty() => {
                let checks: Vec<String> =
                    list.iter().map(|i| format!("$sender == {i}")).collect();
                writeln!(
                    f,
                    "    ({arm_name}, $sender:expr) => {{ {} }};",
                    checks.join(" || ")
                )
                .unwrap();
            }
            _ => {
                writeln!(f, "    ({arm_name}, $sender:expr) => {{ false }};").unwrap();
            }
        }
    }
    // Catch-all for tasks not in config (e.g. cargo check without full build)
    writeln!(f, "    ($name:ident, $sender:expr) => {{ false }};").unwrap();
    writeln!(f, "}}").unwrap();
}

fn write_irqs(out_dir: &PathBuf, config: &Config) {
    let path = out_dir.join("irqs.rs");
    let mut f = std::fs::File::create(&path).unwrap();

    // Per-task IRQ constants. The current task picks its own constants by
    // matching CARGO_PKG_NAME (mirroring how the acl_check macro resolves the
    // current crate). Tasks without IRQs get an empty module.
    writeln!(f, "#[macro_export]").unwrap();
    writeln!(f, "macro_rules! irq_bit {{").unwrap();
    for (task_name, irqs) in &config.task_irqs {
        let arm_name = task_name.replace('-', "_");
        for (irq_name, bit) in irqs {
            writeln!(
                f,
                "    ({arm_name}, {irq_name}) => {{ {bit}u32 }};"
            )
            .unwrap();
        }
    }
    writeln!(
        f,
        "    ($pkg:ident, $irq:ident) => {{ compile_error!(concat!(\"unknown IRQ `\", stringify!($irq), \"` for task `\", stringify!($pkg), \"`\")) }};"
    )
    .unwrap();
    writeln!(f, "}}").unwrap();
}

fn write_irq_stubs(out_dir: &PathBuf) {
    let path = out_dir.join("irqs.rs");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "#[macro_export]").unwrap();
    writeln!(f, "macro_rules! irq_bit {{").unwrap();
    writeln!(f, "    ($pkg:ident, $irq:ident) => {{ 0u32 }};").unwrap();
    writeln!(f, "}}").unwrap();
}

fn write_acl_stubs(out_dir: &PathBuf) {
    let path = out_dir.join("acl.rs");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "#[macro_export]").unwrap();
    writeln!(f, "macro_rules! acl_check {{").unwrap();
    writeln!(f, "    ($name:ident, $sender:expr) => {{ false }};").unwrap();
    writeln!(f, "}}").unwrap();
}

fn write_partitions(out_dir: &PathBuf, config: &Config) {
    let path = out_dir.join("partitions.rs");
    let mut f = std::fs::File::create(&path).unwrap();

    // Partition name constants (null-padded 16-byte arrays)
    writeln!(f, "pub mod names {{").unwrap();
    for part in &config.partitions {
        let const_name = part.name.to_uppercase().replace('-', "_");
        let mut padded = part.name.clone();
        while padded.len() < 16 {
            padded.push('\0');
        }
        let bytes: Vec<String> = padded.bytes().map(|b| format!("{b}")).collect();
        writeln!(f, "    pub const {const_name}: [u8; 16] = [{}];", bytes.join(", ")).unwrap();
    }
    writeln!(f, "}}").unwrap();
    writeln!(f).unwrap();

    // Partition info struct + array
    writeln!(f, "pub struct PartitionInfo {{").unwrap();
    writeln!(f, "    pub name: &'static str,").unwrap();
    writeln!(f, "    pub offset: u64,").unwrap();
    writeln!(f, "    pub size: u64,").unwrap();
    writeln!(f, "}}").unwrap();
    writeln!(f).unwrap();

    writeln!(f, "pub const PARTITIONS: &[PartitionInfo] = &[").unwrap();
    for part in &config.partitions {
        writeln!(
            f,
            "    PartitionInfo {{ name: \"{}\", offset: {}, size: {} }},",
            part.name, part.offset, part.size
        )
        .unwrap();
    }
    writeln!(f, "];").unwrap();
    writeln!(f).unwrap();

    // Filesystem mount table
    writeln!(f, "pub struct FsMount {{").unwrap();
    writeln!(f, "    pub name: &'static str,").unwrap();
    writeln!(f, "    pub source: &'static str,").unwrap();
    writeln!(f, "}}").unwrap();
    writeln!(f).unwrap();

    writeln!(f, "pub const FILESYSTEMS: &[FsMount] = &[").unwrap();
    for fs in &config.filesystems {
        writeln!(
            f,
            "    FsMount {{ name: \"{}\", source: \"{}\" }},",
            fs.mount_name, fs.source
        )
        .unwrap();
    }
    writeln!(f, "];").unwrap();
    writeln!(f).unwrap();

    // Partition ACL
    let has_acl = !config.partition_acl.is_empty();
    let caller_param = if has_acl { "caller" } else { "_caller" };
    writeln!(
        f,
        "pub fn is_partition_allowed(name: &str, {caller_param}: u16) -> bool {{"
    )
    .unwrap();
    writeln!(f, "    match name {{").unwrap();
    for (part_name, allowed) in &config.partition_acl {
        let checks: Vec<String> = allowed.iter().map(|i| format!("caller == {i}")).collect();
        writeln!(f, "        \"{}\" => {},", part_name, checks.join(" || ")).unwrap();
    }
    writeln!(f, "        _ => false,").unwrap();
    writeln!(f, "    }}").unwrap();
    writeln!(f, "}}").unwrap();
}

fn write_partitions_stubs(out_dir: &PathBuf) {
    let path = out_dir.join("partitions.rs");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "pub mod names {{}}").unwrap();
    writeln!(f, "pub struct FsMount {{ pub name: &'static str, pub source: &'static str }}").unwrap();
    writeln!(f, "pub const FILESYSTEMS: &[FsMount] = &[];").unwrap();
    writeln!(
        f,
        "pub fn is_partition_allowed(_name: &str, _caller: u16) -> bool {{ false }}"
    )
    .unwrap();
}

fn write_build_info(out_dir: &PathBuf, config: &Config) {
    let path = out_dir.join("build_info.rs");
    let mut f = std::fs::File::create(&path).unwrap();

    let build_id = config.build_id.as_deref().unwrap_or("unknown");
    let version = config.version.as_deref().unwrap_or("0.0.0");

    let build_id_bytes = uuid::Uuid::parse_str(build_id)
        .map(|u| *u.as_bytes())
        .unwrap_or([0u8; 16]);
    let bytes_literal = format_bytes_array(&build_id_bytes);

    writeln!(f, "pub const BUILD_ID: &str = \"{build_id}\";").unwrap();
    writeln!(f, "pub const VERSION: &str = \"{version}\";").unwrap();
    writeln!(f, "pub const BUILD_ID_BYTES: [u8; 16] = {bytes_literal};").unwrap();
}

fn write_build_info_stubs(out_dir: &PathBuf) {
    let path = out_dir.join("build_info.rs");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "pub const BUILD_ID: &str = \"unknown\";").unwrap();
    writeln!(f, "pub const VERSION: &str = \"0.0.0\";").unwrap();
    writeln!(f, "pub const BUILD_ID_BYTES: [u8; 16] = [0; 16];").unwrap();
}

fn format_bytes_array(bytes: &[u8; 16]) -> String {
    let mut s = String::from("[");
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&format!("0x{b:02x}"));
    }
    s.push(']');
    s
}
