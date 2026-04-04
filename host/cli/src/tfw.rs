use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::PathBuf;

use engine::logs::Logs;
use engine::Backend;
use zip::ZipArchive;

use crate::format::stacktrace::ElfCache;
use crate::metadata::{LogMetadataFile, Species};

pub enum ConnectedBackend {
    Emulator(emulator::Emulator),
    Serial(serial::Serial),
}

impl Backend for ConnectedBackend {
    fn logs(&self) -> &dyn Logs {
        match self {
            ConnectedBackend::Emulator(e) => e.logs(),
            ConnectedBackend::Serial(s) => s.logs(),
        }
    }
}

pub struct TfwMetadata {
    pub species: HashMap<u64, Species>,
    pub type_names: HashMap<u64, String>,
    pub task_names: Vec<String>,
    pub elf_cache: ElfCache,
}

pub fn load_metadata(tfw: &PathBuf) -> TfwMetadata {
    let tfw_bytes = std::fs::read(tfw).expect("failed to read tfw");
    load_metadata_from_bytes(&tfw_bytes)
}

pub fn load_metadata_from_bytes(tfw_bytes: &[u8]) -> TfwMetadata {
    let mut archive = ZipArchive::new(Cursor::new(tfw_bytes)).expect("invalid tfw archive");

    let mut entry = archive
        .by_name("log-metadata.json")
        .expect("tfw missing log-metadata.json");
    let mut json = String::new();
    entry
        .read_to_string(&mut json)
        .expect("failed to read log-metadata.json");
    drop(entry);

    let entries: Vec<LogMetadataFile> =
        serde_json::from_str(&json).expect("failed to parse log-metadata.json");

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

    // Load task ELFs for stack dump resolution
    let mut elf_cache = ElfCache::new();
    let mut archive2 = ZipArchive::new(Cursor::new(tfw_bytes)).expect("invalid tfw archive");
    elf_cache.load_from_archive(&mut archive2);

    TfwMetadata {
        species,
        type_names,
        task_names,
        elf_cache,
    }
}

pub fn parse_backend(spec: &str, tfw: &PathBuf) -> ConnectedBackend {
    if spec == "emulator" {
        ConnectedBackend::Emulator(
            emulator::Emulator::start(tfw)
                .unwrap_or_else(|e| panic!("failed to start emulator: {e}")),
        )
    } else if let Some(ports) = spec.strip_prefix("serial:") {
        let mut parts = ports.split(',');
        let p1 = parts.next().filter(|s| !s.is_empty());
        let p2 = parts.next().filter(|s| !s.is_empty());
        ConnectedBackend::Serial(
            serial::Serial::connect(p1, p2)
                .unwrap_or_else(|e| panic!("failed to connect serial: {e}")),
        )
    } else {
        panic!("unknown backend: {spec} (expected \"emulator\" or \"serial:PORT1,PORT2\")");
    }
}
