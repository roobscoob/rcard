#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use std::io::Write;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let project_root = manifest_dir.parent().unwrap().parent().unwrap();
    let json_path = project_root.join(".work").join("config.json");

    println!("cargo::rerun-if-changed={}", json_path.display());

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("partitions.rs");
    let mut out = std::fs::File::create(&out_path).unwrap();

    if !json_path.exists() {
        writeln!(out, "pub const PARTITIONS: &[PartitionConfig] = &[];").unwrap();
        writeln!(out, "pub const MANAGED_PARTITIONS: &[&str] = &[];").unwrap();
        writeln!(out, "pub const FILESYSTEMS: &[FilesystemMap] = &[];").unwrap();
        writeln!(
            out,
            "pub fn is_partition_allowed(_name: &str, _caller: u16) -> bool {{ true }}"
        )
        .unwrap();
        return;
    }

    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();

    // Partitions from config.json
    writeln!(out, "pub const PARTITIONS: &[PartitionConfig] = &[").unwrap();
    if let Some(parts) = data["partitions"].as_array() {
        for p in parts {
            let name = p["name"].as_str().unwrap();
            let offset = p["offset"].as_u64().unwrap();
            let size = p["size"].as_u64().unwrap();
            let block_size = p["block_size"].as_u64().unwrap();
            writeln!(
                out,
                "    PartitionConfig {{ device: \"mpi2\", name: \"{name}\", \
                 offset_bytes: {offset}, size_bytes: {size}, \
                 erase_size: {block_size}, format: PartitionFormat::Raw }},"
            )
            .unwrap();
        }
    }
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // Managed partitions (partitions used by filesystems)
    let mut managed = Vec::new();
    if let Some(filesystems) = data["filesystems"].as_array() {
        for fs in filesystems {
            if let Some(source) = fs["source"].as_str() {
                managed.push(source.to_string());
            }
        }
    }
    writeln!(out, "pub const MANAGED_PARTITIONS: &[&str] = &[").unwrap();
    for name in &managed {
        writeln!(out, "    \"{name}\",").unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // Filesystem maps
    writeln!(out, "pub const FILESYSTEMS: &[FilesystemMap] = &[").unwrap();
    if let Some(filesystems) = data["filesystems"].as_array() {
        for fs in filesystems {
            let mount_name = fs["mount_name"].as_str().unwrap();
            let source = fs["source"].as_str().unwrap();
            writeln!(
                out,
                "    FilesystemMap {{ filesystem: \"global\", name: \"{mount_name}\", \
                 source_device: \"mpi2\", source_partition: \"{source}\" }},"
            )
            .unwrap();
        }
    }
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // Partition ACL
    let acl = data["partition_acl"].as_object();
    let has_acl = acl.map(|a| !a.is_empty()).unwrap_or(false);
    let caller_param = if has_acl { "caller" } else { "_caller" };
    writeln!(
        out,
        "pub fn is_partition_allowed(name: &str, {caller_param}: u16) -> bool {{"
    )
    .unwrap();
    writeln!(out, "    match name {{").unwrap();
    if let Some(acl) = acl {
        for (part_name, task_indices) in acl {
            let indices: Vec<String> = task_indices
                .as_array()
                .unwrap()
                .iter()
                .map(|i| format!("caller == {}", i.as_u64().unwrap()))
                .collect();
            writeln!(out, "        \"{part_name}\" => {},", indices.join(" || ")).unwrap();
        }
    }
    writeln!(out, "        _ => false,").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
}
