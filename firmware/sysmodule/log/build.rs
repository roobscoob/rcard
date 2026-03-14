#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-env-changed=HUBRIS_TASKS");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // Generate task_names.rs
    let names = std::env::var("HUBRIS_TASKS").unwrap_or("".to_string());
    let tasks: Vec<&str> = names.split(',').collect();

    let mut f = std::fs::File::create(out_dir.join("task_names.rs")).unwrap();
    writeln!(f, "pub static TASK_NAMES: [&str; {}] = [", tasks.len()).unwrap();
    for name in &tasks {
        writeln!(f, "    \"{name}\",").unwrap();
    }
    writeln!(f, "];").unwrap();

    // Generate notifications.rs (GROUP_ID constants)
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let project_root = manifest_dir.parent().unwrap().parent().unwrap();
    let json_path = project_root.join(".work").join("app.notifications.json");

    println!("cargo::rerun-if-changed={}", json_path.display());

    let mut out = std::fs::File::create(out_dir.join("notifications.rs")).unwrap();

    if !json_path.exists() {
        writeln!(out, "pub const GROUP_ID_LOGS: u16 = 0;").unwrap();
        writeln!(out, "pub static LOGS_SUBSCRIBERS: &[u16] = &[];").unwrap();
        return;
    }

    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();

    let groups_obj = data["groups"].as_object().unwrap();
    let mut group_names: Vec<&String> = groups_obj.keys().collect();
    group_names.sort();

    for (gid, group_name) in group_names.iter().enumerate() {
        let screaming = group_name.to_uppercase().replace('-', "_");
        writeln!(out, "#[allow(dead_code)]").unwrap();
        writeln!(out, "pub const GROUP_ID_{screaming}: u16 = {gid};").unwrap();
    }

    // Generate LOGS_SUBSCRIBERS: task indices that subscribe to "logs".
    let mut subscriber_indices: Vec<u16> = Vec::new();
    if let Some(subs) = data["subscribers"].as_object() {
        for (task_name, groups) in subs {
            if groups
                .as_array()
                .is_some_and(|g| g.iter().any(|v| v.as_str() == Some("logs")))
            {
                if let Some(idx) = tasks.iter().position(|t| t == task_name) {
                    subscriber_indices.push(idx as u16);
                }
            }
        }
    }
    writeln!(
        out,
        "pub static LOGS_SUBSCRIBERS: &[u16] = &{:?};",
        subscriber_indices
    )
    .unwrap();
}
