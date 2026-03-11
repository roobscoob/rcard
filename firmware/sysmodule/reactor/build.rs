use std::io::Write;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let project_root = manifest_dir.parent().unwrap().parent().unwrap();
    let json_path = project_root.join(".work").join("app.notifications.json");

    println!("cargo::rerun-if-changed={}", json_path.display());
    println!("cargo::rerun-if-env-changed=HUBRIS_TASKS");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("notifications.rs");
    let mut out = std::fs::File::create(&out_path).unwrap();

    if !json_path.exists() {
        // No notification config — generate stubs for cargo check
        writeln!(out, "pub struct NotificationGroup {{").unwrap();
        writeln!(out, "    pub name: &'static str,").unwrap();
        writeln!(out, "    pub priority_range: core::ops::RangeInclusive<u8>,").unwrap();
        writeln!(out, "}}").unwrap();
        writeln!(out, "pub const GROUP_COUNT: usize = 0;").unwrap();
        writeln!(out, "pub const GROUPS: &[NotificationGroup; 0] = &[];").unwrap();
        writeln!(out, "pub fn is_sender_allowed(_group_id: u16, _sender: u16) -> bool {{ false }}").unwrap();
        writeln!(out, "pub fn group_subscribers(_group_id: u16) -> &'static [u16] {{ &[] }}").unwrap();
        writeln!(out, "pub fn is_subscriber(_group_id: u16, _task: u16) -> bool {{ false }}").unwrap();
        writeln!(out, "pub const SUBSCRIBER_TASKS: &[u16] = &[];").unwrap();
        return;
    }

    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();

    // Parse task name -> task index mapping from HUBRIS_TASKS
    let task_names: Vec<String> = std::env::var("HUBRIS_TASKS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.to_string())
        .collect();

    let task_index = |name: &str| -> Option<usize> {
        task_names.iter().position(|t| t == name)
    };

    // Parse groups in stable alphabetical order
    let groups_obj = data["groups"].as_object().unwrap();
    let mut group_names: Vec<&String> = groups_obj.keys().collect();
    group_names.sort();

    let group_count = group_names.len();

    // --- NotificationGroup struct and GROUPS array ---
    writeln!(out, "#[allow(dead_code)]").unwrap();
    writeln!(out, "pub struct NotificationGroup {{").unwrap();
    writeln!(out, "    pub name: &'static str,").unwrap();
    writeln!(out, "    pub priority_range: core::ops::RangeInclusive<u8>,").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "#[allow(dead_code)]").unwrap();
    writeln!(out, "pub const GROUP_COUNT: usize = {group_count};").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "pub const GROUPS: &[NotificationGroup; {group_count}] = &[").unwrap();
    for name in &group_names {
        let group = &groups_obj[name.as_str()];
        let min_p = group["min_priority"].as_u64().unwrap();
        let max_p = group["max_priority"].as_u64().unwrap();
        writeln!(
            out,
            "    NotificationGroup {{ name: \"{name}\", priority_range: {min_p}..={max_p} }},"
        ).unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // --- Sender ACL: is_sender_allowed(group_id, sender) ---
    let pushers = data["pushers"].as_object();

    let mut any_sender_used = false;
    let mut sender_arms = Vec::new();
    for (gid, group_name) in group_names.iter().enumerate() {
        let mut allowed_indices = Vec::new();
        if let Some(pushers) = pushers {
            for (task_name, groups) in pushers {
                if let Some(groups) = groups.as_array() {
                    for g in groups {
                        if g.as_str() == Some(group_name.as_str()) {
                            if let Some(idx) = task_index(task_name) {
                                allowed_indices.push(idx);
                            }
                        }
                    }
                }
            }
        }
        if allowed_indices.is_empty() {
            sender_arms.push(format!("        {gid} => false,"));
        } else {
            any_sender_used = true;
            let checks: Vec<String> = allowed_indices
                .iter()
                .map(|idx| format!("sender == {idx}"))
                .collect();
            let expr = checks.join(" || ");
            sender_arms.push(format!("        {gid} => {expr},"));
        }
    }
    let sender_param = if any_sender_used { "sender" } else { "_sender" };
    writeln!(out, "pub fn is_sender_allowed(group_id: u16, {sender_param}: u16) -> bool {{").unwrap();
    writeln!(out, "    match group_id {{").unwrap();
    for arm in &sender_arms {
        writeln!(out, "{arm}").unwrap();
    }
    writeln!(out, "        _ => false,").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();

    // --- Subscriber lists per group: group_subscribers(group_id) ---
    let subscribers = data["subscribers"].as_object();

    // Collect all subscriber indices per group, and a global set of all subscribers
    let mut all_subscriber_indices = std::collections::BTreeSet::new();
    let mut per_group_subscribers: Vec<Vec<usize>> = Vec::new();

    for group_name in &group_names {
        let mut indices: Vec<usize> = Vec::new();
        if let Some(subscribers) = subscribers {
            for (task_name, groups) in subscribers {
                if let Some(groups) = groups.as_array() {
                    for g in groups {
                        if g.as_str() == Some(group_name.as_str()) {
                            if let Some(idx) = task_index(task_name) {
                                indices.push(idx);
                                all_subscriber_indices.insert(idx);
                            }
                        }
                    }
                }
            }
        }
        indices.sort();
        per_group_subscribers.push(indices);
    }

    writeln!(out, "pub fn group_subscribers(group_id: u16) -> &'static [u16] {{").unwrap();
    writeln!(out, "    match group_id {{").unwrap();
    for (gid, indices) in per_group_subscribers.iter().enumerate() {
        let vals: Vec<String> = indices.iter().map(|i| format!("{i}")).collect();
        let list = vals.join(", ");
        writeln!(out, "        {gid} => &[{list}],").unwrap();
    }
    writeln!(out, "        _ => &[],").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();

    // --- is_subscriber(group_id, task_index) ---
    let any_subscribers = per_group_subscribers.iter().any(|s| !s.is_empty());
    let task_param = if any_subscribers { "task" } else { "_task" };
    writeln!(out, "pub fn is_subscriber(group_id: u16, {task_param}: u16) -> bool {{").unwrap();
    writeln!(out, "    match group_id {{").unwrap();
    for (gid, indices) in per_group_subscribers.iter().enumerate() {
        if indices.is_empty() {
            writeln!(out, "        {gid} => false,").unwrap();
        } else {
            let checks: Vec<String> = indices
                .iter()
                .map(|idx| format!("task == {idx}"))
                .collect();
            let expr = checks.join(" || ");
            writeln!(out, "        {gid} => {expr},").unwrap();
        }
    }
    writeln!(out, "        _ => false,").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();

    // --- SUBSCRIBER_TASKS: all unique task indices that subscribe to any group ---
    let sub_vals: Vec<String> = all_subscriber_indices.iter().map(|i| format!("{i}")).collect();
    let sub_list = sub_vals.join(", ");
    writeln!(out, "pub const SUBSCRIBER_TASKS: &[u16] = &[{sub_list}];").unwrap();
    writeln!(out).unwrap();

    // --- GROUP_ID_<SCREAMING_NAME> constants for use by #[notification_handler] ---
    for (gid, group_name) in group_names.iter().enumerate() {
        let screaming = group_name.to_uppercase().replace('-', "_");
        writeln!(out, "#[allow(dead_code)]").unwrap();
        writeln!(out, "pub const GROUP_ID_{screaming}: u16 = {gid};").unwrap();
    }
}
