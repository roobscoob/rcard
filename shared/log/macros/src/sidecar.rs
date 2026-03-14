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
    let work_dir = PathBuf::from(&manifest_dir)
        .ancestors()
        .find(|p| p.join(".work").exists())
        .ok_or(())?
        .join(".work");
    Ok(work_dir)
}

fn meta_dir() -> Result<PathBuf, ()> {
    let dir = find_work_dir()?.join("log_meta");
    std::fs::create_dir_all(&dir).map_err(|_| ())?;
    Ok(dir)
}

/// Write a single metadata entry to a sidecar JSON file.
///
/// `filename` should be unique per entry (e.g. based on hash or type name).
/// Silently does nothing if `.work/` doesn't exist (e.g. emulator builds).
pub fn emit(filename: &str, value: &serde_json::Value) {
    let Ok(dir) = meta_dir() else { return };
    let path = dir.join(filename);
    let Ok(content) = serde_json::to_string(value) else {
        return;
    };
    let _ = std::fs::write(path, content);
}
