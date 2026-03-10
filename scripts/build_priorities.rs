/// Generate a `priority_for(task_index: u16) -> i8` function based on the
/// declared client priorities in `app.priorities.json`.
///
/// Usage in a sysmodule's build.rs:
/// ```rust
/// include!("../../scripts/build_priorities.rs");
/// fn main() {
///     emit_priority_for();
///     // ... rest of build.rs
/// }
/// ```
///
/// Include in generated module:
/// ```rust
/// mod generated {
///     include!(concat!(env!("OUT_DIR"), "/priority_for.rs"));
/// }
/// ```
///
/// Pass to ipc::server!:
/// ```rust
/// ipc::server! {
///     @priorities(generated::priority_for)
///     MyResource: MyResourceImpl,
/// }
/// ```
fn emit_priority_for() {
    use std::io::Write;

    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let project_root = manifest_dir
        .ancestors()
        .find(|p| p.join(".work").exists())
        .expect("cannot find project root with .work directory");

    let priorities_path = project_root.join(".work").join("app.priorities.json");

    println!("cargo::rerun-if-changed={}", priorities_path.display());
    println!("cargo::rerun-if-env-changed=HUBRIS_TASKS");

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("priority_for.rs");
    let mut out = std::fs::File::create(&out_path).unwrap();

    if !priorities_path.exists() {
        writeln!(out, "pub fn priority_for(_task_index: u16) -> i8 {{ 0 }}").unwrap();
        return;
    }

    let pkg_name = std::env::var("CARGO_PKG_NAME").unwrap();
    let this_sysmodule = pkg_name
        .strip_suffix("_api")
        .unwrap_or(&pkg_name)
        .to_string();

    let tasks_env = std::env::var("HUBRIS_TASKS").unwrap_or_default();
    let task_list: Vec<&str> = if tasks_env.is_empty() {
        Vec::new()
    } else {
        tasks_env.split(',').collect()
    };

    let priorities_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&priorities_path).unwrap()).unwrap();

    let mut arms: Vec<String> = Vec::new();

    if let Some(obj) = priorities_json.as_object() {
        for (client_task, deps) in obj {
            if let Some(priority) = deps.get(&this_sysmodule).and_then(|v| v.as_i64()) {
                if let Some(idx) = task_list.iter().position(|t| *t == client_task) {
                    arms.push(format!("        {} => {}i8,", idx, priority));
                }
            }
        }
    }

    writeln!(out, "/// Returns the declared priority for the given client task index.").unwrap();
    writeln!(out, "/// Generated from app.priorities.json by build_priorities.rs.").unwrap();
    writeln!(out, "pub fn priority_for(task_index: u16) -> i8 {{").unwrap();
    if arms.is_empty() {
        writeln!(out, "    let _ = task_index;").unwrap();
        writeln!(out, "    0").unwrap();
    } else {
        writeln!(out, "    match task_index {{").unwrap();
        for arm in &arms {
            writeln!(out, "{}", arm).unwrap();
        }
        writeln!(out, "        _ => 0i8,").unwrap();
        writeln!(out, "    }}").unwrap();
    }
    writeln!(out, "}}").unwrap();
}
