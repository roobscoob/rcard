#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use std::io::Write;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let project_root = manifest_dir.parent().unwrap().parent().unwrap();
    let json_path = project_root.join(".work").join("app.partitions.json");

    println!("cargo::rerun-if-changed={}", json_path.display());

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("partitions.rs");
    let mut out = std::fs::File::create(&out_path).unwrap();

    if !json_path.exists() {
        // No partition table — generate empty stubs for cargo check
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

    // Collect all partitions across all devices
    let mut partitions = Vec::new();
    if let Some(devices) = data["devices"].as_object() {
        for (device_name, parts) in devices {
            // Look up per-device block size (erase size)
            let erase_size = data["device_block_sizes"]
                .get(device_name)
                .and_then(|v| v.as_u64())
                .unwrap_or(512);

            if let Some(parts) = parts.as_array() {
                for p in parts {
                    partitions.push((
                        device_name.clone(),
                        p["name"].as_str().unwrap().to_string(),
                        p["offset_bytes"].as_u64().unwrap(),
                        p["size_bytes"].as_u64().unwrap(),
                        p["format"].as_str().unwrap().to_string(),
                        erase_size,
                    ));
                }
            }
        }
    }

    // Collect filesystem mappings
    let mut fs_maps = Vec::new();
    if let Some(filesystems) = data["filesystems"].as_object() {
        for (fs_name, maps) in filesystems {
            if let Some(maps) = maps.as_array() {
                for m in maps {
                    fs_maps.push((
                        fs_name.clone(),
                        m["name"].as_str().unwrap().to_string(),
                        m["source_device"].as_str().unwrap().to_string(),
                        m["source_partition"].as_str().unwrap().to_string(),
                    ));
                }
            }
        }
    }

    writeln!(out, "pub const PARTITIONS: &[PartitionConfig] = &[").unwrap();
    for (device, name, offset_bytes, size_bytes, format, erase_size) in &partitions {
        let fmt_variant = match format.as_str() {
            "boot" => "Boot",
            "raw" => "Raw",
            "ftab" => "Raw",
            "littlefs" => "LittleFs",
            "ringbuffer" => "RingBuffer",
            other => panic!("unknown partition format: {other}"),
        };
        writeln!(
            out,
            "    PartitionConfig {{ device: \"{device}\", name: \"{name}\", \
             offset_bytes: {offset_bytes}, size_bytes: {size_bytes}, \
             erase_size: {erase_size}, \
             format: PartitionFormat::{fmt_variant} }},"
        )
        .unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // Partition names that are managed by a filesystem (cannot be acquired directly)
    let managed: Vec<&str> = fs_maps.iter().map(|(_, _, _, p)| p.as_str()).collect();
    writeln!(out, "pub const MANAGED_PARTITIONS: &[&str] = &[").unwrap();
    for name in &managed {
        writeln!(out, "    \"{name}\",").unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "pub const FILESYSTEMS: &[FilesystemMap] = &[").unwrap();
    for (fs_name, map_name, src_device, src_partition) in &fs_maps {
        writeln!(
            out,
            "    FilesystemMap {{ filesystem: \"{fs_name}\", name: \"{map_name}\", \
             source_device: \"{src_device}\", source_partition: \"{src_partition}\" }},"
        )
        .unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // Generate partition ACL function.
    let mut acl_map: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    if let Some(acl) = data["partition_acl"].as_object() {
        for (task_name, partitions) in acl {
            if let Some(partitions) = partitions.as_array() {
                for p in partitions {
                    let part_name = p.as_str().unwrap().to_string();
                    acl_map
                        .entry(part_name)
                        .or_default()
                        .push(task_name.clone());
                }
            }
        }
    }

    let caller_param = if acl_map.is_empty() {
        "_caller"
    } else {
        "caller"
    };
    writeln!(
        out,
        "pub fn is_partition_allowed(name: &str, {caller_param}: u16) -> bool {{"
    )
    .unwrap();
    if !acl_map.is_empty() {
        writeln!(out, "    use hubris_task_slots::SLOTS;").unwrap();
    }
    writeln!(out, "    match name {{").unwrap();

    for (part_name, task_names) in &acl_map {
        let checks: Vec<String> = task_names
            .iter()
            .map(|t| format!("caller == SLOTS.{t}.task_index()"))
            .collect();
        let expr = checks.join(" || ");
        writeln!(out, "        \"{part_name}\" => {expr},").unwrap();
    }
    writeln!(out, "        _ => false,").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
}
