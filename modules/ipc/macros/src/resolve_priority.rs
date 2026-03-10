//! Compile-time priority resolution.
//!
//! Reads `.work/app.priorities.json` and `HUBRIS_TASKS` to build a mapping
//! from client task index to eviction priority for the current server crate.

use std::path::PathBuf;

/// A (task_index, priority) pair for a single client.
pub struct PriorityEntry {
    pub task_index: usize,
    pub task_name: String,
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
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").map_err(|_| "CARGO_MANIFEST_DIR not set")?;
    let project_root = PathBuf::from(&manifest_dir)
        .ancestors()
        .find(|p| p.join(".work").exists())
        .ok_or("no .work directory found")?
        .to_path_buf();

    let json_path = project_root.join(".work").join("app.priorities.json");
    let content = std::fs::read_to_string(&json_path)
        .map_err(|_| format!("cannot read {}", json_path.display()))?;

    let server_name =
        std::env::var("CARGO_PKG_NAME").map_err(|_| "CARGO_PKG_NAME not set")?;

    // Parse HUBRIS_TASKS: comma-separated task names, position = task index.
    let task_names: Vec<String> = std::env::var("HUBRIS_TASKS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.to_string())
        .collect();

    let task_index = |name: &str| -> Option<usize> {
        task_names.iter().position(|t| t == name)
    };

    // Minimal JSON parsing: the format is:
    // { "client_task": { "sysmodule_x": N, ... }, ... }
    let entries = parse_priorities_for(&content, &server_name, &task_index)?;
    Ok(entries)
}

/// Parse the priorities JSON and extract entries for `server_name`.
///
/// Expected JSON format:
/// ```json
/// {
///   "client_task": {
///     "sysmodule_display": 0,
///     "sysmodule_log": -1
///   }
/// }
/// ```
fn parse_priorities_for(
    json: &str,
    server_name: &str,
    task_index: &dyn Fn(&str) -> Option<usize>,
) -> Result<Vec<PriorityEntry>, String> {
    let mut entries = Vec::new();

    // Find each top-level key (client task name)
    let mut search_from = 0;
    while let Some(pos) = json[search_from..].find('"') {
        let abs_pos = search_from + pos;
        let after_quote = abs_pos + 1;
        let end_quote = match json[after_quote..].find('"') {
            Some(p) => after_quote + p,
            None => break,
        };
        let client_name = &json[after_quote..end_quote];
        search_from = end_quote + 1;

        // Skip to the colon and opening brace
        let rest = json[search_from..].trim_start();
        if !rest.starts_with(':') {
            continue;
        }
        let rest = rest[1..].trim_start();
        if !rest.starts_with('{') {
            continue;
        }

        // Find matching closing brace
        let brace_start = json.len() - rest.len();
        let brace_end = match find_matching_brace(&json[brace_start..]) {
            Some(e) => brace_start + e,
            None => break,
        };
        let inner = &json[brace_start + 1..brace_end];
        search_from = brace_end + 1;

        // Parse the inner object for our server_name
        if let Some(priority) = find_int_value(inner, server_name) {
            if let Some(idx) = task_index(client_name) {
                entries.push(PriorityEntry {
                    task_index: idx,
                    task_name: client_name.to_string(),
                    priority,
                });
            }
        }
    }

    Ok(entries)
}

/// Find a string key in a simple JSON object and return its integer value.
fn find_int_value(json: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{}\"", key);
    let key_pos = json.find(&needle)?;
    let after_key = &json[key_pos + needle.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_colon = after_colon.trim_start();

    // Parse integer (possibly negative)
    let end = after_colon
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(after_colon.len());
    let num_str = &after_colon[..end];
    num_str.parse::<i64>().ok()
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
