use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::compile::CompileArtifact;

/// A collated log metadata bundle, matching the format the CLI expects.
#[derive(Debug, Serialize)]
pub struct LogMetadataBundle {
    pub task_names: Vec<String>,
    pub types: BTreeMap<String, serde_json::Value>,
    pub fields: BTreeMap<String, serde_json::Value>,
    pub species: BTreeMap<String, SpeciesEntry>,
}

#[derive(Debug, Serialize)]
pub struct SpeciesEntry {
    pub format: String,
    pub arg_count: u32,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// Raw sidecar entry as written by the proc macro.
#[derive(Debug, Deserialize)]
struct SidecarFile {
    id: String,
    entry: SidecarEntry,
}

#[derive(Debug, Deserialize)]
struct SidecarEntry {
    kind: String,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    arg_count: Option<u32>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    column: Option<u32>,
    // Types/fields may have other fields — capture as raw JSON
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

/// Scrape log metadata from `.log_strings` ELF sections in task artifacts
/// and produce a collated bundle.
pub fn scrape(
    task_names: &[String],
    artifacts: &[CompileArtifact],
) -> Result<LogMetadataBundle, MetadataError> {
    use object::read::Object;
    use object::ObjectSection;

    let mut types = BTreeMap::new();
    let mut fields = BTreeMap::new();
    let mut species = BTreeMap::new();

    for artifact in artifacts {
        let data = match std::fs::read(&artifact.elf_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let obj = match object::File::parse(&*data) {
            Ok(o) => o,
            Err(_) => continue,
        };

        let section = match obj.section_by_name(".log_strings") {
            Some(s) => s,
            None => continue,
        };
        let section_data = match section.data() {
            Ok(d) => d,
            Err(_) => continue,
        };

        // Each entry is null-terminated JSON
        for chunk in section_data.split(|&b| b == 0) {
            if chunk.is_empty() {
                continue;
            }
            let sidecar: SidecarFile = match serde_json::from_slice(chunk) {
                Ok(s) => s,
                Err(_) => continue,
            };

            match sidecar.entry.kind.as_str() {
                "struct" | "enum" | "variant" => {
                    let mut value = serde_json::Map::new();
                    for (k, v) in &sidecar.entry.extra {
                        value.insert(k.clone(), v.clone());
                    }
                    value.insert("kind".into(), sidecar.entry.kind.clone().into());
                    types.insert(sidecar.id, serde_json::Value::Object(value));
                }
                "field" => {
                    let mut value = serde_json::Map::new();
                    for (k, v) in &sidecar.entry.extra {
                        value.insert(k.clone(), v.clone());
                    }
                    value.insert("kind".into(), sidecar.entry.kind.clone().into());
                    fields.insert(sidecar.id, serde_json::Value::Object(value));
                }
                "species" => {
                    let entry = SpeciesEntry {
                        format: sidecar.entry.format.unwrap_or_default(),
                        arg_count: sidecar.entry.arg_count.unwrap_or(0),
                        kind: "species".to_string(),
                        file: sidecar.entry.file.map(|f| crate::shorten_path(&f)),
                        line: sidecar.entry.line,
                        column: sidecar.entry.column,
                    };

                    species.insert(sidecar.id, entry);
                }
                _ => {}
            }
        }
    }

    Ok(LogMetadataBundle {
        task_names: task_names.to_vec(),
        types,
        fields,
        species,
    })
}


/// Write the collated log metadata to a JSON file.
pub fn emit(bundle: &LogMetadataBundle, out_path: &Path) -> Result<(), MetadataError> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(MetadataError::Io)?;
    }

    // Wrap in array to match existing CLI format: Vec<LogMetadataFile>
    let json = serde_json::to_string_pretty(&vec![bundle]).map_err(MetadataError::Json)?;
    std::fs::write(out_path, json).map_err(MetadataError::Io)?;

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    #[error("metadata IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("metadata JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
