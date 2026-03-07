use std::io::Write;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let project_root = manifest_dir.parent().unwrap().parent().unwrap().parent().unwrap();
    let json_path = project_root.join(".work").join("app.partitions.json");

    println!("cargo::rerun-if-changed={}", json_path.display());

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("partition_names.rs");
    let mut out = std::fs::File::create(&out_path).unwrap();

    if !json_path.exists() {
        // No partition table — generate empty module for cargo check
        writeln!(out, "pub mod partitions {{}}").unwrap();
        return;
    }

    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();

    writeln!(out, "pub mod partitions {{").unwrap();

    if let Some(devices) = data["devices"].as_object() {
        for parts in devices.values() {
            if let Some(parts) = parts.as_array() {
                for p in parts {
                    let name = p["name"].as_str().unwrap();
                    let const_name = name.to_uppercase().replace('-', "_");
                    let mut padded = String::from(name);
                    while padded.len() < 16 {
                        padded.push('\0');
                    }
                    let bytes: Vec<String> = padded.bytes().map(|b| format!("{b}")).collect();
                    let bytes_str = bytes.join(", ");
                    writeln!(out, "    pub const {const_name}: [u8; 16] = [{bytes_str}];").unwrap();
                }
            }
        }
    }

    writeln!(out, "}}").unwrap();
}
