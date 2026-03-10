use std::io::Write;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let project_root = manifest_dir.parent().unwrap().parent().unwrap();
    let json_path = project_root.join(".work").join("app.partitions.json");

    println!("cargo::rerun-if-changed={}", json_path.display());

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("filesystem_config.rs");
    let mut out = std::fs::File::create(&out_path).unwrap();

    if !json_path.exists() {
        writeln!(out, "pub(crate) const AUTO_MOUNT: &[AutoMount] = &[];").unwrap();
        return;
    }

    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();

    // Collect filesystem mappings — these are partitions fs should auto-mount
    let mut mounts = Vec::new();
    if let Some(filesystems) = data["filesystems"].as_object() {
        for (_fs_name, maps) in filesystems {
            if let Some(maps) = maps.as_array() {
                for m in maps {
                    mounts.push((
                        m["name"].as_str().unwrap().to_string(),
                        m["source_partition"].as_str().unwrap().to_string(),
                    ));
                }
            }
        }
    }

    writeln!(out, "pub(crate) const AUTO_MOUNT: &[AutoMount] = &[").unwrap();
    for (name, partition) in &mounts {
        writeln!(
            out,
            "    AutoMount {{ name: \"{name}\", partition: \"{partition}\" }},"
        )
        .unwrap();
    }
    writeln!(out, "];").unwrap();
}
