#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use std::io::Write;
use std::path::PathBuf;

fn main() {
    // Prefer TFW_CONFIG_JSON (set by `tfw::build::build` to the real
    // per-build work dir). Fall back to `firmware/.work/config.json`
    // for direct cargo check.
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
    let out_path = out_dir.join("partitions.rs");
    let mut out = std::fs::File::create(&out_path).unwrap();

    if !json_path.exists() {
        writeln!(out, "pub const PARTITIONS: &[PartitionConfig] = &[];").unwrap();
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

    // Partitions that back filesystem mounts get PART_MANAGED set —
    // mirrors the flag the host writes into places.bin so the two
    // sources stay in sync.
    let managed_set: std::collections::HashSet<String> = data["filesystems"]
        .as_array()
        .map(|fs| {
            fs.iter()
                .filter_map(|f| f["source"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Partitions from config.json. Geometry (erase_size etc.) is queried
    // from the MPI driver at runtime, not baked per partition.
    writeln!(out, "pub const PARTITIONS: &[PartitionConfig] = &[").unwrap();
    if let Some(parts) = data["partitions"].as_array() {
        for p in parts {
            let name = p["name"].as_str().unwrap();
            let offset = p["offset"].as_u64().unwrap();
            let size = p["size"].as_u64().unwrap();
            let mut flags: u32 = 0;
            if managed_set.contains(name) {
                flags |= rcard_places::PART_MANAGED;
            }
            writeln!(
                out,
                "    PartitionConfig {{ device: \"mpi2\", name: \"{name}\", \
                 offset_bytes: {offset}, size_bytes: {size}, \
                 flags: {flags}, format: PartitionFormat::Raw }},"
            )
            .unwrap();
        }
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
