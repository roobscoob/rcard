//! Scrape `#[ipc::resource]` / `#[ipc::interface]` / `ipc::server!`
//! metadata from `.ipc_meta` ELF sections in compiled task artifacts.
//!
//! Emitted by [`ipc_macros::section`](../../../firmware/modules/ipc/macros/src/section.rs):
//! each call site writes a null-terminated JSON record into `.ipc_meta`
//! via a `#[link_section]` static. This scraper reads the section from
//! every task ELF, parses each chunk as JSON, and collates into a
//! single [`IpcMetadataBundle`] for writing as `ipc-metadata.json`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::compile::CompileArtifact;

/// Top-level collated IPC metadata for a single firmware build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcMetadataBundle {
    /// Every resource and interface trait reachable from any task ELF.
    /// Keyed by trait name so multiple task ELFs referencing the same
    /// api crate collapse to one entry.
    pub resources: BTreeMap<String, ResourceEntry>,
    /// Every `ipc::server!` call site, keyed by task name. Tells the
    /// host which task serves which resource traits.
    pub servers: BTreeMap<String, ServerEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceEntry {
    /// "resource" or "interface".
    #[serde(rename = "type")]
    pub ty: String,
    pub name: String,
    pub kind: u8,
    #[serde(default)]
    pub arena_size: Option<usize>,
    #[serde(default)]
    pub clone: Option<String>,
    #[serde(default)]
    pub implements: Option<String>,
    pub methods: Vec<MethodEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodEntry {
    pub id: u8,
    pub kind: String,
    pub name: String,
    pub params: Vec<ParamEntry>,
    #[serde(default)]
    pub return_type: Option<String>,
    #[serde(default)]
    pub ctor_return: Option<serde_json::Value>,
    #[serde(default)]
    pub constructs: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub is_lease: bool,
    #[serde(default)]
    pub lease_mutable: bool,
    #[serde(default)]
    pub handle_mode: Option<String>,
    #[serde(default)]
    pub impl_trait: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    pub task: String,
    pub serves: Vec<String>,
}

/// Scrape `.ipc_meta` sections from every task ELF and collate.
pub fn scrape(artifacts: &[CompileArtifact]) -> Result<IpcMetadataBundle, IpcMetadataError> {
    use object::ObjectSection;
    use object::read::Object;

    let mut resources: BTreeMap<String, ResourceEntry> = BTreeMap::new();
    let mut servers: BTreeMap<String, ServerEntry> = BTreeMap::new();

    for artifact in artifacts {
        let data = match std::fs::read(&artifact.elf_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let obj = match object::File::parse(&*data) {
            Ok(o) => o,
            Err(_) => continue,
        };

        let section = match obj.section_by_name(".ipc_meta") {
            Some(s) => s,
            None => continue,
        };
        let section_data = match section.data() {
            Ok(d) => d,
            Err(_) => continue,
        };

        for chunk in section_data.split(|&b| b == 0) {
            if chunk.is_empty() {
                continue;
            }
            let value: serde_json::Value = match serde_json::from_slice(chunk) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let ty = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match ty {
                "resource" | "interface" => {
                    let entry: ResourceEntry = match serde_json::from_value(value) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    // First-write wins on collision; every emission of
                    // the same trait is identical (same api crate).
                    resources.entry(entry.name.clone()).or_insert(entry);
                }
                "server" => {
                    let entry: ServerEntry = match serde_json::from_value(value) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    servers.entry(entry.task.clone()).or_insert(entry);
                }
                _ => {}
            }
        }
    }

    Ok(IpcMetadataBundle {
        resources,
        servers,
    })
}

/// Write the bundle to a JSON file (pretty-printed).
pub fn emit(bundle: &IpcMetadataBundle, out_path: &Path) -> Result<(), IpcMetadataError> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(IpcMetadataError::Io)?;
    }
    let json = serde_json::to_string_pretty(bundle).map_err(IpcMetadataError::Json)?;
    std::fs::write(out_path, json).map_err(IpcMetadataError::Io)?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum IpcMetadataError {
    #[error("ipc metadata IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ipc metadata JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
