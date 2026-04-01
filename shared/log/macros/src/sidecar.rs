//! Write log metadata to sidecar JSON files in `.work/log_meta/`.
//!
//! This replaces the old approach of embedding metadata in a `.rcard_log` ELF
//! section. By writing to files instead, the metadata never ends up in the
//! binary and costs zero bytes of flash.
//!
//! The pattern mirrors `ipc/macros/src/emit_meta.rs`.

use std::path::PathBuf;

fn find_work_dir() -> Result<PathBuf, ()> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| ())?;
    for ancestor in PathBuf::from(&manifest_dir).ancestors() {
        let direct = ancestor.join(".work");
        if direct.exists() {
            return Ok(direct);
        }
        // Repo root: check firmware/.work for crates outside firmware/
        let nested = ancestor.join("firmware").join(".work");
        if nested.exists() {
            return Ok(nested);
        }
    }
    Err(())
}

fn meta_dir() -> Result<PathBuf, ()> {
    let dir = find_work_dir()?.join("log_meta");
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("failed to create {}: {e}", dir.display()));
    Ok(dir)
}

/// Write a single metadata entry to a sidecar JSON file.
///
/// `filename` should be unique per entry (e.g. based on hash or type name).
/// Silently does nothing if `.work/` doesn't exist (e.g. rust-analyzer,
/// host builds). Panics on write errors during real builds so missing
/// species metadata is caught immediately.
pub fn emit(filename: &str, value: &serde_json::Value) {
    let Ok(dir) = meta_dir() else { return };
    let path = dir.join(filename);
    let content = serde_json::to_string(value)
        .unwrap_or_else(|e| panic!("failed to serialize log metadata: {e}"));
    std::fs::write(&path, content)
        .unwrap_or_else(|e| panic!("failed to write log metadata to {}: {e}", path.display()));
}
