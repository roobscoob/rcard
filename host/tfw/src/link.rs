use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use object::read::elf::{ElfFile32, FileHeader, ProgramHeader};
use object::Endianness;

use crate::compile::CompileArtifact;
use crate::config::AppConfig;
use crate::layout::Layout;

/// Combine multiple task ELFs + kernel ELF into a places binary.
///
/// Groups all PT_LOAD segments by place (using the layout's address ranges),
/// merges segments within each place into one contiguous blob, and writes
/// a `places.bin` using the `rcard_places` format.
pub fn link_image(
    artifacts: &[CompileArtifact],
    config: &AppConfig,
    layout: &Layout,
    out_dir: &Path,
) -> Result<PathBuf, LinkError> {
    std::fs::create_dir_all(out_dir).map_err(LinkError::Io)?;

    let places_path = out_dir.join("places.bin");

    // Build a lookup: (cpu_base, cpu_end) → place_name for all CPU-mapped places.
    let place_ranges = build_place_ranges(config);

    // Collect all PT_LOAD segments from all ELFs.
    let segments = collect_segments(artifacts)?;

    if segments.is_empty() {
        return Err(LinkError::NoSegments);
    }

    // Group segments by place.
    let mut by_place: BTreeMap<&str, Vec<&Segment>> = BTreeMap::new();
    for seg in &segments {
        let place_name = find_place(&place_ranges, seg.paddr)
            .ok_or_else(|| LinkError::UnmappedSegment {
                owner: seg.owner.clone(),
                addr: seg.paddr,
            })?;
        by_place.entry(place_name).or_default().push(seg);
    }

    // Kernel entry point = kernel code region base address.
    let kernel_code = layout.placed
        .get(&("kernel".to_string(), "code".to_string()))
        .ok_or_else(|| LinkError::Other("kernel.code not in layout".into()))?;
    let entry_point = kernel_code.base as u32;

    // Build the places binary.
    let mut builder = rcard_places::PlacesBuilder::new(entry_point);

    // Emit partition table entries for all flash places.
    for (place_name, place) in &config.places {
        if let Some(offset) = place.offset {
            let flags = if place.unmapped { rcard_places::PART_UNMAPPED } else { 0 };
            builder.add_partition(
                rcard_places::name_hash(place_name.as_bytes()),
                offset as u32,
                place.size as u32,
                flags,
            );
        }
    }

    for (place_name, segs) in &by_place {
        let mut segs = segs.clone();
        segs.sort_by_key(|s| s.paddr);

        // Check for overlaps within this place.
        for pair in segs.windows(2) {
            let a = pair[0];
            let b = pair[1];
            if a.paddr + a.data.len() as u64 > b.paddr {
                return Err(LinkError::Overlap {
                    a_owner: a.owner.clone(),
                    a_addr: a.paddr,
                    b_owner: b.owner.clone(),
                    b_addr: b.paddr,
                });
            }
        }

        let base = segs.first().unwrap().paddr;
        let end = segs.iter().map(|s| s.paddr + s.data.len() as u64).max().unwrap();
        let total = (end - base) as usize;

        // Merge into a contiguous blob (gaps filled with 0xFF).
        let mut blob = vec![0xFFu8; total];
        for seg in &segs {
            let offset = (seg.paddr - base) as usize;
            blob[offset..offset + seg.data.len()].copy_from_slice(&seg.data);
        }

        // mem_size: include .bss regions in this place.
        // Find all placed regions that fall within this place's range.
        let (place_start, place_end) = place_ranges.iter()
            .find(|(_, name)| *name == *place_name)
            .map(|((s, e), _)| (*s, *e))
            .unwrap();

        let mem_end = layout.placed.values()
            .filter(|a| a.base >= place_start && a.base < place_end)
            .map(|a| a.base + a.size)
            .max()
            .unwrap_or(end);

        let mem_size = (mem_end - base) as u32;

        eprintln!("    place {place_name}: {:#010x} ({total} bytes file, {mem_size} bytes mem)",
            base);

        builder.add_segment(base as u32, &blob, mem_size);
    }

    let places_bin = builder.build();
    std::fs::write(&places_path, &places_bin).map_err(LinkError::Io)?;

    Ok(places_path)
}

/// Extract a flat binary from an ELF (like `objcopy -O binary`).
/// Used for the bootloader which runs XIP from flash.
pub fn extract_flat_binary(
    artifact: &CompileArtifact,
    out_dir: &Path,
) -> Result<PathBuf, LinkError> {
    let out_path = out_dir.join("bootloader.bin");
    std::fs::create_dir_all(out_dir).map_err(LinkError::Io)?;

    let data = std::fs::read(&artifact.elf_path).map_err(LinkError::Io)?;
    let elf = ElfFile32::<Endianness>::parse(&*data).map_err(|e| LinkError::Elf {
        crate_name: artifact.crate_name.clone(),
        message: e.to_string(),
    })?;

    let endian = elf.endian();
    let phdrs = elf.elf_header().program_headers(endian, elf.data()).map_err(|e| {
        LinkError::Elf {
            crate_name: artifact.crate_name.clone(),
            message: e.to_string(),
        }
    })?;

    let mut min_addr = u64::MAX;
    let mut max_addr = 0u64;
    let mut segments = Vec::new();

    for header in phdrs {
        if header.p_type(endian) != object::elf::PT_LOAD {
            continue;
        }
        let filesz = header.p_filesz(endian) as usize;
        if filesz == 0 {
            continue;
        }
        let offset = header.p_offset(endian) as usize;
        let paddr = header.p_paddr(endian) as u64;
        if offset < 64 {
            continue;
        }

        let seg_data = data
            .get(offset..offset + filesz)
            .ok_or_else(|| LinkError::Elf {
                crate_name: artifact.crate_name.clone(),
                message: format!(
                    "segment at offset {offset:#x} size {filesz:#x} extends past EOF"
                ),
            })?;

        min_addr = min_addr.min(paddr);
        max_addr = max_addr.max(paddr + filesz as u64);
        segments.push((paddr, seg_data));
    }

    if segments.is_empty() {
        return Err(LinkError::NoSegments);
    }

    let total = (max_addr - min_addr) as usize;
    let mut bin = vec![0xFFu8; total];
    for (addr, seg_data) in &segments {
        let off = (*addr - min_addr) as usize;
        bin[off..off + seg_data.len()].copy_from_slice(seg_data);
    }

    eprintln!("    bootloader: {:#010x} ({total} bytes)", min_addr);
    std::fs::write(&out_path, &bin).map_err(LinkError::Io)?;
    Ok(out_path)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

struct Segment {
    paddr: u64,
    data: Vec<u8>,
    owner: String,
}

/// Build a sorted list of (cpu_start, cpu_end) → place_name for all CPU-mapped places.
fn build_place_ranges(config: &AppConfig) -> Vec<((u64, u64), &str)> {
    let mut ranges = Vec::new();
    for (name, place) in &config.places {
        if place.unmapped || place.mappings.is_empty() {
            continue;
        }
        let offset = place.offset.unwrap_or(0);
        for mapping in &place.mappings {
            let start = mapping.address + offset;
            let end = start + place.size;
            ranges.push(((start, end), name.as_str()));
        }
    }
    ranges.sort_by_key(|((s, _), _)| *s);
    ranges
}

/// Find which place a physical address belongs to.
fn find_place<'a>(ranges: &'a [((u64, u64), &str)], addr: u64) -> Option<&'a str> {
    for ((start, end), name) in ranges {
        if addr >= *start && addr < *end {
            return Some(name);
        }
    }
    None
}

/// Collect all PT_LOAD segments from all artifact ELFs.
fn collect_segments(artifacts: &[CompileArtifact]) -> Result<Vec<Segment>, LinkError> {
    let mut segments = Vec::new();

    for artifact in artifacts {
        let data = std::fs::read(&artifact.elf_path).map_err(LinkError::Io)?;
        let elf = ElfFile32::<Endianness>::parse(&*data).map_err(|e| LinkError::Elf {
            crate_name: artifact.crate_name.clone(),
            message: e.to_string(),
        })?;

        let endian = elf.endian();
        let phdrs = elf.elf_header().program_headers(endian, elf.data()).map_err(|e| {
            LinkError::Elf {
                crate_name: artifact.crate_name.clone(),
                message: e.to_string(),
            }
        })?;

        for header in phdrs {
            if header.p_type(endian) != object::elf::PT_LOAD {
                continue;
            }

            let filesz = header.p_filesz(endian) as usize;
            if filesz == 0 {
                continue;
            }

            let offset = header.p_offset(endian) as usize;
            let paddr = header.p_paddr(endian) as u64;

            // Skip the ELF header segment (offset 0).
            if offset < 64 {
                continue;
            }

            let seg_data = data
                .get(offset..offset + filesz)
                .ok_or_else(|| LinkError::Elf {
                    crate_name: artifact.crate_name.clone(),
                    message: format!(
                        "segment at offset {offset:#x} size {filesz:#x} extends past EOF"
                    ),
                })?;

            segments.push(Segment {
                paddr,
                data: seg_data.to_vec(),
                owner: artifact.crate_name.clone(),
            });
        }
    }

    Ok(segments)
}

// ── Errors ───────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("link IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ELF error in {crate_name}: {message}")]
    Elf { crate_name: String, message: String },
    #[error("no LOAD segments found in any artifact")]
    NoSegments,
    #[error("segment overlap: {a_owner} @ {a_addr:#010x} overlaps {b_owner} @ {b_addr:#010x}")]
    Overlap {
        a_owner: String,
        a_addr: u64,
        b_owner: String,
        b_addr: u64,
    },
    #[error("segment from {owner} at {addr:#010x} doesn't fall in any place")]
    UnmappedSegment { owner: String, addr: u64 },
    #[error("{0}")]
    Other(String),
}
