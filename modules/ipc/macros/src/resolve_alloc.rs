//! Compile-time allocation resolution.
//!
//! Reads `.work/app.allocations.json` to look up the base address and size
//! of a named allocation.

use std::path::PathBuf;

pub struct AllocInfo {
    pub base: u64,
    pub size: u64,
    pub align: u64,
}

/// Look up the allocation named `alloc_name`.
///
/// Returns:
/// - `Ok(Some(info))` — allocation found
/// - `Ok(None)` — JSON exists but allocation not found
/// - `Err(msg)` — file missing or unparseable
pub fn resolve(alloc_name: &str) -> Result<Option<AllocInfo>, String> {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").map_err(|_| "CARGO_MANIFEST_DIR not set")?;
    let project_root = PathBuf::from(&manifest_dir)
        .ancestors()
        .find(|p| p.join(".work").exists())
        .ok_or("cannot find project root with .work directory")?
        .to_path_buf();

    let json_path = project_root.join(".work").join("app.allocations.json");
    let content = match std::fs::read_to_string(&json_path) {
        Ok(c) => c,
        Err(_) => return Err(format!("cannot read {}", json_path.display())),
    };

    // Minimal JSON parsing: find "allocations" -> alloc_name -> {base, size}
    let allocs_key = "\"allocations\"";
    let allocs_pos = match content.find(allocs_key) {
        Some(p) => p,
        None => return Ok(None),
    };

    let after_allocs = &content[allocs_pos + allocs_key.len()..];
    let after_colon = match after_allocs.trim_start().strip_prefix(':') {
        Some(s) => s.trim_start(),
        None => return Ok(None),
    };

    // Find the specific allocation object
    let needle = format!("\"{}\"", alloc_name);
    let name_pos = match after_colon.find(&needle) {
        Some(p) => p,
        None => return Ok(None),
    };

    let after_name = &after_colon[name_pos + needle.len()..];
    let obj_start = match after_name.find('{') {
        Some(p) => p,
        None => return Ok(None),
    };

    let obj_content = &after_name[obj_start..];
    let obj_end = match find_matching_brace(obj_content) {
        Some(p) => p,
        None => return Ok(None),
    };

    let obj = &obj_content[..=obj_end];

    let base = extract_number(obj, "base").ok_or_else(|| {
        format!("allocation '{}': missing 'base' field", alloc_name)
    })?;
    let size = extract_number(obj, "size").ok_or_else(|| {
        format!("allocation '{}': missing 'size' field", alloc_name)
    })?;
    let align = extract_number(obj, "align").unwrap_or(1);

    Ok(Some(AllocInfo { base, size, align }))
}

/// Check whether the current crate has `uses-allocation` for `alloc_name`.
///
/// Returns:
/// - `Ok(Some(message))` — violation
/// - `Ok(None)` — allowed or skipped
/// - `Err(msg)` — file missing
pub fn check_acl(alloc_name: &str) -> Result<Option<String>, String> {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").map_err(|_| "CARGO_MANIFEST_DIR not set".to_string())?;
    let project_root = PathBuf::from(&manifest_dir)
        .ancestors()
        .find(|p| p.join(".work").exists())
        .ok_or("cannot find project root")?
        .to_path_buf();

    let json_path = project_root.join(".work").join("app.allocations.json");
    let content = match std::fs::read_to_string(&json_path) {
        Ok(c) => c,
        Err(_) => return Err(format!("cannot read {}", json_path.display())),
    };

    let pkg_name = std::env::var("CARGO_PKG_NAME")
        .map_err(|_| "CARGO_PKG_NAME not set".to_string())?;

    // Find "acl" section
    let acl_key = "\"acl\"";
    let acl_pos = match content.find(acl_key) {
        Some(p) => p,
        None => return Ok(None), // no ACL section, skip
    };

    let after_acl = &content[acl_pos + acl_key.len()..];
    let after_colon = match after_acl.trim_start().strip_prefix(':') {
        Some(s) => s.trim_start(),
        None => return Ok(None),
    };

    // Find the task's entry in the ACL
    let task_needle = format!("\"{}\"", pkg_name);
    let task_pos = match after_colon.find(&task_needle) {
        Some(p) => p,
        // Task not in ACL at all — it has no allocation grants
        None => {
            return Ok(Some(format!(
                "task '{}' does not have `uses-allocation \"{}\"` in app.kdl",
                pkg_name, alloc_name,
            )));
        }
    };

    // Find the array of allocations for this task
    let after_task = &after_colon[task_pos + task_needle.len()..];
    let arr_start = match after_task.find('[') {
        Some(p) => p,
        None => return Ok(None),
    };
    let arr_content = &after_task[arr_start..];
    let arr_end = match find_matching_bracket(arr_content) {
        Some(p) => p,
        None => return Ok(None),
    };
    let arr = &arr_content[..=arr_end];

    // Check if the allocation name appears in the array
    let alloc_needle = format!("\"{}\"", alloc_name);
    if arr.contains(&alloc_needle) {
        Ok(None) // found, all good
    } else {
        Ok(Some(format!(
            "task '{}' does not have `uses-allocation \"{}\"` in app.kdl",
            pkg_name, alloc_name,
        )))
    }
}

fn find_matching_bracket(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, ch) in s.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_number(json_obj: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{}\"", key);
    let pos = json_obj.find(&needle)?;
    let after = &json_obj[pos + needle.len()..];
    let after_colon = after.trim_start().strip_prefix(':')?;
    let after_colon = after_colon.trim_start();

    // Parse the number (may end at comma, brace, or whitespace)
    let end = after_colon
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after_colon.len());
    after_colon[..end].parse().ok()
}

fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0;
    for (i, ch) in s.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}
