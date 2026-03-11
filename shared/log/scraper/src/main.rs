use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use clap::Parser;
use object::read::elf::{ElfFile32, FileHeader, SectionHeader};
use object::LittleEndian;

#[derive(Parser)]
#[command(name = "rcard-log-scraper")]
#[command(about = "Extract .rcard_log metadata from task ELF files")]
struct Cli {
    /// Path to the ELF file to scrape
    elf_path: PathBuf,

    /// Optional output JSON path (defaults to stdout)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Task name (defaults to ELF filename)
    #[arg(short, long)]
    task: Option<String>,
}

#[derive(serde::Serialize)]
struct Bundle {
    task: String,
    types: BTreeMap<String, serde_json::Value>,
    fields: BTreeMap<String, serde_json::Value>,
    species: BTreeMap<String, serde_json::Value>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let data = fs::read(&cli.elf_path)?;
    let elf = ElfFile32::<LittleEndian>::parse(&*data)?;

    let task_name = cli
        .task
        .unwrap_or_else(|| {
            cli.elf_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".into())
        });

    let mut bundle = Bundle {
        task: task_name,
        types: BTreeMap::new(),
        fields: BTreeMap::new(),
        species: BTreeMap::new(),
    };

    // Find the .rcard_log section
    let endian = LittleEndian;
    let header = elf.elf_header();
    let sections = header.sections(endian, &*data)?;

    for section in sections.iter() {
        let name = sections.section_name(endian, section)?;
        if name != b".rcard_log" {
            continue;
        }

        let section_data = section.data(endian, &*data)?;
        let section_vaddr = section.sh_addr(endian) as u64;

        // Split on null bytes, tracking offsets
        let mut offset = 0;
        while offset < section_data.len() {
            // Skip padding nulls
            if section_data[offset] == 0 {
                offset += 1;
                continue;
            }

            // Find the end of this null-terminated entry
            let start = offset;
            while offset < section_data.len() && section_data[offset] != 0 {
                offset += 1;
            }

            let entry_bytes = &section_data[start..offset];
            let entry_addr = section_vaddr + start as u64;
            let addr_hex = format!("0x{:08x}", entry_addr);

            // Skip the null terminator
            if offset < section_data.len() {
                offset += 1;
            }

            // Parse as JSON
            let entry_str = match std::str::from_utf8(entry_bytes) {
                Ok(s) => s,
                Err(_) => {
                    eprintln!("warning: non-UTF8 entry at {addr_hex}, skipping");
                    continue;
                }
            };

            let value: serde_json::Value = match serde_json::from_str(entry_str) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("warning: invalid JSON at {addr_hex}: {e}");
                    continue;
                }
            };

            // Categorize by "kind" field
            match value.get("kind").and_then(|k| k.as_str()) {
                Some("struct") | Some("enum") | Some("variant") => {
                    bundle.types.insert(addr_hex, value);
                }
                Some("field") => {
                    bundle.fields.insert(addr_hex, value);
                }
                Some("species") => {
                    bundle.species.insert(addr_hex, value);
                }
                Some(other) => {
                    eprintln!("warning: unknown kind '{other}' at {addr_hex}");
                }
                None => {
                    eprintln!("warning: entry at {addr_hex} has no 'kind' field");
                }
            }
        }
    }

    let json = serde_json::to_string_pretty(&bundle)?;

    match cli.output {
        Some(path) => fs::write(path, json)?,
        None => println!("{json}"),
    }

    Ok(())
}
