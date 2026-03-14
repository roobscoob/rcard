#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-env-changed=HUBRIS_TASKS");

    let names = std::env::var("HUBRIS_TASKS").unwrap_or_default();

    let tasks: Vec<&str> = if names.is_empty() {
        vec![]
    } else {
        names.split(',').collect()
    };

    let mut path = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    path.push("task_names.rs");

    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "pub static TASK_NAMES: [&str; {}] = [", tasks.len()).unwrap();
    for name in &tasks {
        writeln!(f, "    \"{name}\",").unwrap();
    }
    writeln!(f, "];").unwrap();
}
