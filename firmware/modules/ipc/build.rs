use std::io::Write;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-env-changed=HUBRIS_TASKS");

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
