//! Compile-time ACL resolution.
//!
//! Reads `.work/app.uses.json` and `.work/app.peers.json` together with
//! `HUBRIS_TASKS` to build a list of task indices that are allowed to send
//! IPC messages to the current server crate.

/// Resolve the ACL for the server crate identified by `CARGO_PKG_NAME`.
///
/// Returns a sorted, deduplicated list of task indices that are authorized
/// to send messages to this server.
///
/// Returns `Err` if the required files are missing or unparseable (e.g.,
/// not running under the Hubris build system). Callers should treat `Err`
/// as "skip enforcement" rather than a hard error.
pub fn resolve() -> Result<Vec<u16>, String> {
    let work_dir = crate::resolve_alloc::find_work_dir()?;

    let uses_path = work_dir.join("app.uses.json");
    let uses_content = std::fs::read_to_string(&uses_path)
        .map_err(|_| format!("cannot read {}", uses_path.display()))?;

    let peers_path = work_dir.join("app.peers.json");
    let peers_content = std::fs::read_to_string(&peers_path)
        .map_err(|_| format!("cannot read {}", peers_path.display()))?;

    let server_name =
        std::env::var("CARGO_PKG_NAME").map_err(|_| "CARGO_PKG_NAME not set")?;

    // Parse HUBRIS_TASKS: comma-separated task names, position = task index.
    let task_names: Vec<String> = std::env::var("HUBRIS_TASKS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.to_string())
        .collect();

    let task_index_of = |name: &str| -> Option<u16> {
        task_names
            .iter()
            .position(|t| t == name)
            .map(|i| i as u16)
    };

    // JSON structure: { "client_task": ["server_task", ...], ... }
    let uses: serde_json::Value =
        serde_json::from_str(&uses_content).map_err(|e| format!("bad uses JSON: {e}"))?;
    let uses_obj = uses.as_object().ok_or("uses JSON is not an object")?;

    // JSON structure: { "server_task": ["peer_task", ...], ... }
    let peers: serde_json::Value =
        serde_json::from_str(&peers_content).map_err(|e| format!("bad peers JSON: {e}"))?;
    let peers_obj = peers.as_object().ok_or("peers JSON is not an object")?;

    let mut allowed: Vec<u16> = Vec::new();

    // Invert uses: for each client that lists this server as a dependency,
    // add the client's task index.
    for (client_name, deps) in uses_obj {
        let deps = deps.as_array().ok_or("expected array per client in uses JSON")?;
        let lists_us = deps
            .iter()
            .any(|d| d.as_str() == Some(&server_name));
        if lists_us {
            if let Some(idx) = task_index_of(client_name) {
                allowed.push(idx);
            }
        }
    }

    // Add peers (bidirectional).
    // Forward: peers of this server.
    if let Some(our_peers) = peers_obj.get(&server_name).and_then(|v| v.as_array()) {
        for peer in our_peers {
            if let Some(peer_name) = peer.as_str() {
                if let Some(idx) = task_index_of(peer_name) {
                    allowed.push(idx);
                }
            }
        }
    }
    // Reverse: servers that list us as a peer.
    for (other_server, their_peers) in peers_obj {
        if other_server == &server_name {
            continue;
        }
        let their_peers = their_peers.as_array().ok_or("expected array per server in peers JSON")?;
        let lists_us = their_peers
            .iter()
            .any(|p| p.as_str() == Some(&server_name));
        if lists_us {
            if let Some(idx) = task_index_of(other_server) {
                allowed.push(idx);
            }
        }
    }

    allowed.sort_unstable();
    allowed.dedup();
    Ok(allowed)
}
