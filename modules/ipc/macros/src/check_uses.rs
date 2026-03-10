//! Compile-time dependency enforcement.
//!
//! Reads `.work/app.uses.json` to check whether the consuming crate has
//! declared a `uses-sysmodule` dependency on the target task.

use std::path::PathBuf;

/// Check whether the current crate (from `CARGO_PKG_NAME`) is allowed to
/// use `dep_task`.
///
/// Returns:
/// - `Ok(Some(message))` — violation: the consumer doesn't declare the dep
/// - `Ok(None)` — either allowed, or enforcement is skipped
/// - `Err(())` — file missing or unparseable; skip enforcement
pub fn check(dep_task: &str) -> Result<Option<String>, ()> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| ())?;
    let project_root = PathBuf::from(&manifest_dir)
        .ancestors()
        .find(|p| p.join(".work").exists())
        .ok_or(())?
        .to_path_buf();

    let json_path = project_root.join(".work").join("app.uses.json");
    let content = std::fs::read_to_string(&json_path).map_err(|_| ())?;

    let pkg_name = std::env::var("CARGO_PKG_NAME").map_err(|_| ())?;

    // JSON structure: { "task_name": ["dep1", "dep2"], ... }
    let root: serde_json::Value = serde_json::from_str(&content).map_err(|_| ())?;
    let obj = root.as_object().ok_or(())?;

    // If the package isn't listed at all, it might be a non-task crate — skip.
    let deps = match obj.get(&pkg_name) {
        Some(v) => v.as_array().ok_or(())?,
        None => return Ok(None),
    };

    if deps.iter().any(|d| d.as_str() == Some(dep_task)) {
        Ok(None) // dependency declared, all good
    } else {
        let short_name = dep_task.strip_prefix("sysmodule_").unwrap_or(dep_task);
        Ok(Some(format!(
            "This task does not declare a dependency on `{}`. \
             Add `uses-sysmodule \"{}\"` to your task in app.kdl.",
            dep_task, short_name,
        )))
    }
}
