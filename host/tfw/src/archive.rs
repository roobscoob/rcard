use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::Path;

use zip::ZipArchive;

use crate::build_metadata::BuildMetadata;
use crate::config::AppConfig;
use crate::elf_cache::ElfCache;
use crate::metadata::{LogMetadataFile, Species};

pub struct TfwMetadata {
    pub species: HashMap<u64, Species>,
    pub type_names: HashMap<u64, String>,
    pub task_names: Vec<String>,
    pub elf_cache: ElfCache,
    pub build: Option<BuildMetadata>,
    pub config: Option<AppConfig>,
}

#[derive(Debug)]
pub enum ArchiveError {
    Io(std::io::Error),
    Zip(zip::result::ZipError),
    Json(serde_json::Error),
    MissingEntry(&'static str),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::Io(e) => write!(f, "archive IO error: {e}"),
            ArchiveError::Zip(e) => write!(f, "archive ZIP error: {e}"),
            ArchiveError::Json(e) => write!(f, "archive JSON error: {e}"),
            ArchiveError::MissingEntry(name) => write!(f, "tfw missing {name}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

pub fn load_metadata(tfw: &Path) -> Result<TfwMetadata, ArchiveError> {
    let tfw_bytes = std::fs::read(tfw).map_err(ArchiveError::Io)?;
    load_metadata_from_bytes(&tfw_bytes)
}

pub fn load_metadata_from_bytes(tfw_bytes: &[u8]) -> Result<TfwMetadata, ArchiveError> {
    let mut archive = ZipArchive::new(Cursor::new(tfw_bytes)).map_err(ArchiveError::Zip)?;

    let mut entry = archive
        .by_name("log-metadata.json")
        .map_err(|_| ArchiveError::MissingEntry("log-metadata.json"))?;
    let mut json = String::new();
    entry
        .read_to_string(&mut json)
        .map_err(ArchiveError::Io)?;
    drop(entry);

    let entries: Vec<LogMetadataFile> =
        serde_json::from_str(&json).map_err(ArchiveError::Json)?;

    let mut species = HashMap::new();
    let mut type_names = HashMap::new();
    let mut task_names = Vec::new();

    for entry in &entries {
        for (hash_str, sp) in &entry.species {
            if let Ok(hash) = u64::from_str_radix(hash_str.trim_start_matches("0x"), 16) {
                species.insert(hash, sp.clone());
            }
        }
        for (hash_str, value) in &entry.types {
            let Ok(hash) = u64::from_str_radix(hash_str.trim_start_matches("0x"), 16) else {
                continue;
            };
            if let Some(name) = value.get("name").and_then(|n| n.as_str()) {
                type_names.insert(hash, name.to_string());
            }
        }
        if task_names.is_empty() {
            task_names.clone_from(&entry.task_names);
        }
    }

    // Load build metadata (optional — older archives may not have it).
    let build = {
        let mut archive3 = ZipArchive::new(Cursor::new(tfw_bytes)).map_err(ArchiveError::Zip)?;
        match archive3.by_name("build-metadata.json") {
            Ok(mut entry) => {
                let mut json = String::new();
                entry.read_to_string(&mut json).ok();
                serde_json::from_str(&json).ok()
            }
            Err(_) => None,
        }
    };

    // Load app config (optional — always present in practice).
    let config: Option<AppConfig> = {
        let mut archive4 = ZipArchive::new(Cursor::new(tfw_bytes)).map_err(ArchiveError::Zip)?;
        match archive4.by_name("config.json") {
            Ok(mut entry) => {
                let mut json = String::new();
                entry.read_to_string(&mut json).ok();
                serde_json::from_str(&json).ok()
            }
            Err(_) => None,
        }
    };

    // Load task ELFs for stack dump resolution
    let mut elf_cache = ElfCache::new();
    let mut archive2 = ZipArchive::new(Cursor::new(tfw_bytes)).map_err(ArchiveError::Zip)?;
    elf_cache.load_from_archive(&mut archive2);

    Ok(TfwMetadata {
        species,
        type_names,
        task_names,
        elf_cache,
        build,
        config,
    })
}
