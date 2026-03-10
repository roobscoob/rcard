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

pub(crate) fn find_work_dir() -> Result<PathBuf, String> {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").map_err(|_| "CARGO_MANIFEST_DIR not set")?;
    let work_dir = PathBuf::from(&manifest_dir)
        .ancestors()
        .find(|p| p.join(".work").exists())
        .ok_or("cannot find project root with .work directory")?
        .join(".work");
    Ok(work_dir)
}

fn read_alloc_json() -> Result<serde_json::Value, String> {
    let json_path = find_work_dir()?.join("app.allocations.json");
    let content = std::fs::read_to_string(&json_path)
        .map_err(|_| format!("cannot read {}", json_path.display()))?;
    serde_json::from_str(&content).map_err(|e| format!("bad allocations JSON: {e}"))
}

/// Look up the allocation named `alloc_name`.
///
/// Returns:
/// - `Ok(Some(info))` — allocation found
/// - `Ok(None)` — JSON exists but allocation not found
/// - `Err(msg)` — file missing or unparseable
pub fn resolve(alloc_name: &str) -> Result<Option<AllocInfo>, String> {
    let root = read_alloc_json()?;

    let alloc = match root.get("allocations").and_then(|a| a.get(alloc_name)) {
        Some(v) => v,
        None => return Ok(None),
    };

    let base = alloc
        .get("base")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| format!("allocation '{}': missing 'base' field", alloc_name))?;
    let size = alloc
        .get("size")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| format!("allocation '{}': missing 'size' field", alloc_name))?;
    let align = alloc.get("align").and_then(|v| v.as_u64()).unwrap_or(1);

    Ok(Some(AllocInfo { base, size, align }))
}

/// Check whether the current crate has `uses-allocation` for `alloc_name`.
///
/// Returns:
/// - `Ok(Some(message))` — violation
/// - `Ok(None)` — allowed or skipped
/// - `Err(msg)` — file missing
pub fn check_acl(alloc_name: &str) -> Result<Option<String>, String> {
    let root = read_alloc_json()?;

    let pkg_name = std::env::var("CARGO_PKG_NAME")
        .map_err(|_| "CARGO_PKG_NAME not set".to_string())?;

    let acl = match root.get("acl") {
        Some(v) => v,
        None => return Ok(None),
    };

    let task_grants = match acl.get(&pkg_name) {
        Some(v) => v.as_array().ok_or("acl entry is not an array")?,
        None => {
            return Ok(Some(format!(
                "task '{}' does not have `uses-allocation \"{}\"` in app.kdl",
                pkg_name, alloc_name,
            )));
        }
    };

    if task_grants
        .iter()
        .any(|v| v.as_str() == Some(alloc_name))
    {
        Ok(None)
    } else {
        Ok(Some(format!(
            "task '{}' does not have `uses-allocation \"{}\"` in app.kdl",
            pkg_name, alloc_name,
        )))
    }
}
