#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-env-changed=HUBRIS_TASKS");

    // Track build-system JSON files so servers recompile when ACLs/priorities change.
    if let Some(work_dir) = find_work_dir() {
        for file in ["app.uses.json", "app.peers.json", "app.priorities.json"] {
            println!("cargo::rerun-if-changed={}", work_dir.join(file).display());
        }
    }

    let task_count = std::env::var("HUBRIS_TASKS")
        .map(|s| s.split(',').filter(|t| !t.is_empty()).count())
        .unwrap_or(0);

    let mut path = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    path.push("task_count.rs");

    let mut f = std::fs::File::create(path).unwrap();
    writeln!(
        f,
        "/// Total number of tasks in the system, set by the build system."
    )
    .unwrap();
    writeln!(
        f,
        "/// Zero if HUBRIS_TASKS is not set (e.g. during `cargo check`)."
    )
    .unwrap();
    writeln!(f, "pub const TASK_COUNT: usize = {task_count};").unwrap();
}

fn find_work_dir() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").ok()?);
    manifest_dir
        .ancestors()
        .find(|p| p.join(".work").exists())
        .map(|p| p.join(".work"))
}
