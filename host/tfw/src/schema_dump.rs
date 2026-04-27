//! Generate and run a temporary Rust project that imports firmware api
//! crates on the host target, accesses their `__ipc_schema_*::RESOURCE`
//! consts, and dumps method manifests as JSON to stdout.
//!
//! The generated project is written to `<work_dir>/ipc-schema-dumper/`
//! with a shared `CARGO_TARGET_DIR` so incremental builds are fast
//! after the first run.

use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::build::{BuildEvent, HostCrateEvent, HostCrateState, ResourceUpdate};

/// An api crate to include in the schema dump.
pub struct ApiCrate {
    /// Cargo package name (e.g. `sysmodule_log_api`).
    pub package: String,
    /// Absolute path to the crate's directory (containing Cargo.toml).
    pub path: PathBuf,
    /// The resource trait names defined in this crate (e.g. `["Log"]`).
    /// Used to construct the `__ipc_schema_<lower>` module names.
    pub resource_names: Vec<String>,
}

/// Build and run the schema dump, streaming cargo messages via `emit`.
/// Returns the JSON output from the binary's stdout.
pub fn run(
    api_crates: &[ApiCrate],
    work_dir: &Path,
    emit: &dyn Fn(BuildEvent),
) -> Result<String, SchemaDumpError> {
    let project_dir = work_dir.join("ipc-schema-dumper");
    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir).map_err(SchemaDumpError::Io)?;

    write_cargo_toml(&project_dir, api_crates)?;
    write_main_rs(&src_dir, api_crates)?;

    let target_dir = project_dir.join("target");

    emit(BuildEvent::HostCrate {
        name: "ipc-schema-dumper".into(),
        update: ResourceUpdate::State(HostCrateState::Building),
    });

    let build = escargot::CargoBuild::new()
        .manifest_path(project_dir.join("Cargo.toml"))
        .release()
        .env("CARGO_TARGET_DIR", target_dir.display().to_string());

    let messages = build
        .exec()
        .map_err(|e| SchemaDumpError::Other(format!("failed to run cargo: {e:#}")))?;

    let mut failed = false;
    let mut bin_path: Option<PathBuf> = None;

    for msg in messages {
        match msg {
            Ok(msg) => {
                if let Ok(val) = msg.decode_custom::<serde_json::Value>() {
                    if let Some(exe) = val.get("executable").and_then(|v| v.as_str()) {
                        bin_path = Some(PathBuf::from(exe));
                    }
                }
                emit(BuildEvent::HostCrate {
                    name: "ipc-schema-dumper".into(),
                    update: ResourceUpdate::Event(HostCrateEvent::CargoMessage(msg)),
                });
            }
            Err(e) => {
                emit(BuildEvent::HostCrate {
                    name: "ipc-schema-dumper".into(),
                    update: ResourceUpdate::Event(HostCrateEvent::CargoError(e)),
                });
                failed = true;
            }
        }
    }

    if failed {
        return Err(SchemaDumpError::CargoFailed(
            "cargo build failed (see diagnostics above)".into(),
        ));
    }

    let bin = bin_path.ok_or_else(|| {
        SchemaDumpError::Other("cargo build produced no binary artifact".into())
    })?;

    emit(BuildEvent::HostCrate {
        name: "ipc-schema-dumper".into(),
        update: ResourceUpdate::State(HostCrateState::Running),
    });

    let output = Command::new(&bin)
        .output()
        .map_err(SchemaDumpError::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(SchemaDumpError::RunFailed(stderr));
    }

    emit(BuildEvent::HostCrate {
        name: "ipc-schema-dumper".into(),
        update: ResourceUpdate::State(HostCrateState::Done),
    });

    String::from_utf8(output.stdout)
        .map_err(|e| SchemaDumpError::Other(format!("non-UTF8 output: {e}")))
}

fn write_cargo_toml(
    project_dir: &Path,
    api_crates: &[ApiCrate],
) -> Result<(), SchemaDumpError> {
    let mut s = String::new();
    writeln!(s, "[package]").unwrap();
    writeln!(s, "name = \"ipc-schema-dumper\"").unwrap();
    writeln!(s, "version = \"0.1.0\"").unwrap();
    writeln!(s, "edition = \"2021\"").unwrap();
    writeln!(s).unwrap();
    // Opt out of the parent workspace — the tmp project lives under
    // the host workspace's target dir and cargo would otherwise try
    // to include it.
    writeln!(s, "[workspace]").unwrap();
    writeln!(s).unwrap();
    writeln!(s, "[dependencies]").unwrap();
    writeln!(s, "serde_json = \"1\"").unwrap();

    // Each api crate is depended on with the `host` feature, which
    // forwards to `ipc/host` — enabling Serialize on ResourceDesc
    // and OwnedNamedType conversion. No direct dep on `ipc` needed.
    for api in api_crates {
        let path_str = api.path.display().to_string().replace('\\', "/");
        writeln!(
            s,
            "{} = {{ path = \"{}\", features = [\"host\"] }}",
            api.package, path_str
        )
        .unwrap();
    }

    std::fs::write(project_dir.join("Cargo.toml"), &s).map_err(SchemaDumpError::Io)
}

fn write_main_rs(src_dir: &Path, api_crates: &[ApiCrate]) -> Result<(), SchemaDumpError> {
    let mut s = String::new();
    writeln!(s, "fn main() {{").unwrap();
    writeln!(s, "    let mut resources: Vec<serde_json::Value> = Vec::new();").unwrap();

    for api in api_crates {
        for resource in &api.resource_names {
            let mod_name = format!("__ipc_schema_{}", resource.to_lowercase());
            writeln!(s).unwrap();
            writeln!(
                s,
                "    resources.push(serde_json::to_value(&{pkg}::{mod_name}::RESOURCE).unwrap());",
                pkg = api.package,
            )
            .unwrap();
        }
    }

    writeln!(s).unwrap();
    writeln!(
        s,
        "    println!(\"{{}}\", serde_json::to_string_pretty(&resources).unwrap());"
    )
    .unwrap();
    writeln!(s, "}}").unwrap();

    std::fs::write(src_dir.join("main.rs"), &s).map_err(SchemaDumpError::Io)
}

#[derive(Debug)]
pub enum SchemaDumpError {
    Io(std::io::Error),
    CargoFailed(String),
    RunFailed(String),
    Other(String),
}

impl std::fmt::Display for SchemaDumpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "schema dump IO error: {e}"),
            Self::CargoFailed(stderr) => write!(f, "schema dump cargo failed:\n{stderr}"),
            Self::RunFailed(stderr) => write!(f, "schema dump binary failed:\n{stderr}"),
            Self::Other(msg) => write!(f, "schema dump error: {msg}"),
        }
    }
}

impl std::error::Error for SchemaDumpError {}
