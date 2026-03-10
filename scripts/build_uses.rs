/// Emit `cargo:rustc-env=HUBRIS_USES_<TASK>=1` for each real dependency of
/// this task, so the bind macro can verify dependencies at compile time.
///
/// Also emits `HUBRIS_USES_CHECKED=1` as a sentinel so the bind macro knows
/// enforcement is active. When `app.uses.json` doesn't exist (e.g. during
/// plain `cargo check` in the IDE), no sentinel is emitted and enforcement
/// is silently skipped.
///
/// Call from build.rs. Reads `.work/app.uses.json` and looks up the
/// current crate's task name (derived from `CARGO_PKG_NAME`).
///
/// Usage in build.rs:
/// ```rust
/// include!("../../scripts/build_uses.rs");
/// fn main() {
///     emit_uses_cfg();
///     // ... rest of build.rs
/// }
/// ```
fn emit_uses_cfg() {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let project_root = manifest_dir
        .ancestors()
        .find(|p| p.join(".work").exists())
        .expect("cannot find project root with .work directory");
    let json_path = project_root.join(".work").join("app.uses.json");

    println!("cargo::rerun-if-changed={}", json_path.display());

    if !json_path.exists() {
        return;
    }

    // Sentinel: tells the bind macro that dependency info is available.
    println!("cargo::rustc-env=HUBRIS_USES_CHECKED=1");

    let pkg_name = std::env::var("CARGO_PKG_NAME").unwrap();

    let data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();

    if let Some(deps) = data.get(&pkg_name).and_then(|v| v.as_array()) {
        for dep in deps {
            if let Some(name) = dep.as_str() {
                let env_name = format!("HUBRIS_USES_{}", name.to_uppercase());
                println!("cargo::rustc-env={env_name}=1");
            }
        }
    }
}
