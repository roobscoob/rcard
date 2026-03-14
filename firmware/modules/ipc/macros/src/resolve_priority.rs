//! Compile-time priority resolution.
//!
//! Reads `.work/app.priorities.json` and `HUBRIS_TASKS` to build a mapping
//! from client task index to eviction priority for the current server crate.

/// A (task_index, priority) pair for a single client.
pub struct PriorityEntry {
    pub task_index: usize,
    pub priority: i64,
}

/// Resolve priorities for the server crate identified by `CARGO_PKG_NAME`.
///
/// Returns a list of (task_index, task_name, priority) entries for all
/// clients that declared a `with-priority` for this server.
///
/// Returns `Err` if the required files are missing or unparseable (e.g.,
/// not running under the Hubris build system). Callers should treat `Err`
/// as "skip enforcement" rather than a hard error.
pub fn resolve() -> Result<Vec<PriorityEntry>, String> {
    let json_path = crate::resolve_alloc::find_work_dir()?.join("app.priorities.json");
    let content = std::fs::read_to_string(&json_path)
        .map_err(|_| format!("cannot read {}", json_path.display()))?;

    let server_name = std::env::var("CARGO_PKG_NAME").map_err(|_| "CARGO_PKG_NAME not set")?;

    // Parse HUBRIS_TASKS: comma-separated task names, position = task index.
    let task_names: Vec<String> = std::env::var("HUBRIS_TASKS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.to_string())
        .collect();

    // JSON structure: { "client_task": { "sysmodule_x": N, ... }, ... }
    let root: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("bad priorities JSON: {e}"))?;
    let obj = root.as_object().ok_or("priorities JSON is not an object")?;

    let mut entries = Vec::new();
    for (client_name, inner) in obj {
        let inner = inner.as_object().ok_or("expected object per client")?;
        if let Some(priority) = inner.get(&server_name).and_then(|v| v.as_i64()) {
            if let Some(idx) = task_names.iter().position(|t| t == client_name) {
                entries.push(PriorityEntry {
                    task_index: idx,
                    priority,
                });
            }
        }
    }

    Ok(entries)
}
