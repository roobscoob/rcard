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

    // Check if this package is even a task in the JSON.
    // If the package isn't listed at all, it might be a non-task crate — skip.
    let deps = parse_deps_for(&content, &pkg_name);

    // If the task isn't in the JSON at all, skip enforcement.
    let deps = match deps {
        Some(d) => d,
        None => return Ok(None),
    };

    if deps.iter().any(|d| d == dep_task) {
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

/// Minimal JSON parser: extract the string array for a given key.
///
/// Expects format like: `{"pkg_a": ["dep1", "dep2"], "pkg_b": ["dep3"]}`
fn parse_deps_for(json: &str, key: &str) -> Option<Vec<String>> {
    // Find "key": [...]
    let needle = format!("\"{}\"", key);
    let key_pos = json.find(&needle)?;
    let after_key = &json[key_pos + needle.len()..];

    // Skip whitespace and colon
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_colon = after_colon.trim_start();

    // Find the array
    if !after_colon.starts_with('[') {
        return None;
    }

    let bracket_end = find_matching_bracket(after_colon)?;
    let array_content = &after_colon[1..bracket_end];

    // Extract string values
    let mut deps = Vec::new();
    let mut remaining = array_content;
    while let Some(quote_start) = remaining.find('"') {
        let after_quote = &remaining[quote_start + 1..];
        let quote_end = after_quote.find('"')?;
        deps.push(after_quote[..quote_end].to_string());
        remaining = &after_quote[quote_end + 1..];
    }

    Some(deps)
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
