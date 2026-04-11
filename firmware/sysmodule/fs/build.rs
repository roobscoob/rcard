#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let project_root = manifest_dir.parent().unwrap().parent().unwrap();
    let json_path = project_root.join(".work").join("config.json");

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

    writeln!(out, "pub(crate) const AUTO_MOUNT: &[AutoMount] = &[").unwrap();
    if let Some(filesystems) = data["filesystems"].as_array() {
        for fs in filesystems {
            let name = fs["mount_name"].as_str().unwrap();
            let source = fs["source"].as_str().unwrap();
            writeln!(
                out,
                "    AutoMount {{ name: \"{name}\", partition: \"{source}\" }},"
            )
            .unwrap();
        }
    }
    writeln!(out, "];").unwrap();
}
