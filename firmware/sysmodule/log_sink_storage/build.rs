#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let project_root = manifest_dir.parent().unwrap().parent().unwrap();
    let json_path = project_root.join(".work").join("app.notifications.json");

    println!("cargo::rerun-if-changed={}", json_path.display());

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("notifications.rs");
    let mut out = std::fs::File::create(&out_path).unwrap();

    if !json_path.exists() {
        // No notification config — generate stub for cargo check
        writeln!(out, "pub const GROUP_ID_LOGS: u16 = 0;").unwrap();
        return;
    }

    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();

    // Parse groups in stable alphabetical order (must match reactor's build.rs)
    let groups_obj = data["groups"].as_object().unwrap();
    let mut group_names: Vec<&String> = groups_obj.keys().collect();
    group_names.sort();

    for (gid, group_name) in group_names.iter().enumerate() {
        let screaming = group_name.to_uppercase().replace('-', "_");
        writeln!(out, "#[allow(dead_code)]").unwrap();
        writeln!(out, "pub const GROUP_ID_{screaming}: u16 = {gid};").unwrap();
    }
}
