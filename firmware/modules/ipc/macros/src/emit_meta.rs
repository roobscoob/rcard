//! Post-build metadata emission for handle ACL verification.
//!
//! Each `#[resource]`, `#[interface]`, and `server!` invocation writes a
//! small JSON file into `.work/ipc_meta/`. After compilation, a separate
//! checker reads these files and cross-references them against
//! `app.uses.json` and `app.peers.json` to verify that every
//! handle-passing site has the necessary IPC permissions.

pub struct HandleParam {
    pub method: String,
    pub handle_trait: String,
    pub mode: String, // "move" or "clone"
}

fn meta_dir() -> Result<std::path::PathBuf, ()> {
    let work_dir = crate::resolve_alloc::find_work_dir().map_err(|_| ())?;
    let dir = work_dir.join("ipc_meta");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        println!("cargo::error=ipc: failed to create ipc_meta dir: {e}");
        return Err(());
    }
    Ok(dir)
}

/// Emit metadata for a `#[ipc::resource]` trait.
pub fn emit_resource(
    crate_name: &str,
    trait_name: &str,
    kind: u8,
    implements: Option<&str>,
    clone_mode: Option<&str>,
    handle_params: &[HandleParam],
) {
    let Ok(dir) = meta_dir() else { return };

    let params: Vec<serde_json::Value> = handle_params
        .iter()
        .map(|p| {
            serde_json::json!({
                "method": p.method,
                "handle_trait": p.handle_trait,
                "mode": p.mode,
            })
        })
        .collect();

    let val = serde_json::json!({
        "crate": crate_name,
        "trait_name": trait_name,
        "kind": kind,
        "is_interface": false,
        "implements": implements,
        "clone_mode": clone_mode,
        "handle_params": params,
    });

    let path = dir.join(format!("resource.{}.{}.json", crate_name, trait_name));
    if let Err(e) = write_json(&path, &val) {
        println!(
            "cargo::warning=ipc: failed to write {}: {e}",
            path.display()
        );
    }
}

/// Emit metadata for a `#[ipc::interface]` trait.
pub fn emit_interface(crate_name: &str, trait_name: &str, kind: u8) {
    let Ok(dir) = meta_dir() else { return };

    let val = serde_json::json!({
        "crate": crate_name,
        "trait_name": trait_name,
        "kind": kind,
        "is_interface": true,
        "implements": null,
        "clone_mode": null,
        "handle_params": [],
    });

    let path = dir.join(format!("resource.{}.{}.json", crate_name, trait_name));
    if let Err(e) = write_json(&path, &val) {
        println!(
            "cargo::warning=ipc: failed to write {}: {e}",
            path.display()
        );
    }
}

/// Emit metadata for a `server!` invocation.
pub fn emit_server(task_name: &str, serves: &[String]) {
    let Ok(dir) = meta_dir() else { return };

    let val = serde_json::json!({
        "task": task_name,
        "serves": serves,
    });

    let path = dir.join(format!("server.{}.json", task_name));
    if let Err(e) = write_json(&path, &val) {
        println!(
            "cargo::warning=ipc: failed to write {}: {e}",
            path.display()
        );
    }
}

fn write_json(path: &std::path::Path, val: &serde_json::Value) -> std::io::Result<()> {
    let content = serde_json::to_string_pretty(val)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(path, content)
}
