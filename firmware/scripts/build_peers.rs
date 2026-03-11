/// Emit `cargo:rustc-cfg=peer="<task_name>"` for each peer of this task.
///
/// Call from build.rs. Reads `.work/app.peers.json` and looks up the
/// current crate's task name (derived from `CARGO_PKG_NAME`).
///
/// Usage in build.rs:
/// ```rust
/// include!("../../scripts/build_peers.rs");
/// fn main() {
///     emit_peer_cfg();
///     // ... rest of build.rs
/// }
/// ```
fn emit_peer_cfg() {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let project_root = manifest_dir
        .ancestors()
        .find(|p| p.join(".work").exists())
        .expect("cannot find project root with .work directory");
    let json_path = project_root.join(".work").join("app.peers.json");

    println!("cargo::rerun-if-changed={}", json_path.display());

    if !json_path.exists() {
        return;
    }

    let pkg_name = std::env::var("CARGO_PKG_NAME").unwrap();

    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();

    if let Some(peers) = data.get(&pkg_name).and_then(|v| v.as_array()) {
        for peer in peers {
            if let Some(name) = peer.as_str() {
                println!("cargo::rustc-cfg=peer=\"{name}\"");
            }
        }
    }
}
