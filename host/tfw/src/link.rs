use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use object::read::elf::{ElfFile32, FileHeader, ProgramHeader};
use object::Endianness;

use crate::compile::CompileArtifact;
use crate::config::AppConfig;
use crate::layout::Layout;

/// Where a place's bytes ended up inside `places.bin`.
///
/// `file_offset` is the byte offset within `places.bin` where this
/// place's blob starts. `blob_base` is the lowest paddr among the
/// segments packed into the blob — i.e. the linker address that maps
/// to byte 0 of the blob. Together they let callers convert a paddr
/// in this place to its absolute flash byte address (e.g. for
/// populating ftab entries that drive the BOOTROM copy path).
#[derive(Debug, Clone)]
pub struct PlaceLayout {
    pub file_offset: u32,
    pub blob_base: u64,
}

/// Combine multiple task ELFs + kernel ELF into a places binary.
///
/// Groups all PT_LOAD segments by place (using the layout's address ranges),
/// merges segments within each place into one contiguous blob, and writes
/// a `places.bin` using the `rcard_places` format.
///
/// Returns the path to the written file plus a `place_name → PlaceLayout`
/// map so downstream consumers (notably `pack::build_ftab_from_config`)
/// can derive the on-flash byte address of a region whose linker address
/// lives in another place (e.g. the bootloader, linked into SRAM but
/// physically packed into the firmware partition).
pub fn link_image(
    artifacts: &[CompileArtifact],
    config: &AppConfig,
    layout: &Layout,
    out_dir: &Path,
    emit: crate::build::EventFn<'_>,
) -> Result<(PathBuf, BTreeMap<String, PlaceLayout>), LinkError> {
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

    // Places that back filesystem mounts — marked PART_MANAGED so the
    // storage sysmodule rejects direct acquires from non-fs tasks.
    let fs_sources: HashSet<&str> = config
        .filesystems
        .values()
        .flat_map(|fs| fs.mounts.iter().map(|m| m.source.as_str()))
        .collect();

    // Emit partition table entries for all flash places.
    for (place_name, place) in &config.places {
        if let Some(offset) = place.offset {
            let mut flags = 0;
            if place.unmapped {
                flags |= rcard_places::PART_UNMAPPED;
            }
            if fs_sources.contains(place_name.as_str()) {
                flags |= rcard_places::PART_MANAGED;
            }
            builder.add_partition(
                rcard_places::name_hash(place_name.as_bytes()),
                offset as u32,
                place.size as u32,
                flags,
            );
        }
    }

    // CPU base of the place hosting places.bin. Segments in this place
    // get file_offset = (segment_dest - host_base), so their bytes land
    // at their linker addresses on flash and run XIP. Segments destined
    // for other places (RAM-init data) are packed after the host place's
    // data, at any unclaimed file offset.
    //
    // Which place hosts places.bin is chosen by the app ncl via
    // `boot.image`. Without a boot config there is no host place.
    let host_place_name: Option<&str> = config.boot.as_ref()
        .and_then(|b| b.image.name.as_deref());
    let host_base = config.boot.as_ref()
        .and_then(|b| b.image.mappings.first().map(|m| m.address + b.image.offset.unwrap_or(0)));

    // Iterate places in two passes: host place first (so it takes
    // file_offset 0..host_size), then everything else packed after.
    let mut place_names: Vec<&str> = by_place.keys().copied().collect();
    place_names.sort_by_key(|n| if Some(*n) == host_place_name { 0 } else { 1 });

    let mut place_layouts: BTreeMap<String, PlaceLayout> = BTreeMap::new();
    let mut tail_cursor: u32 = 0;
    for place_name in place_names {
        let mut segs = by_place[place_name].clone();
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

        // Merge PT_LOADs in this place into one contiguous blob
        // (0xFF gap-fill).
        let mut blob = vec![0xFFu8; total];
        for seg in &segs {
            let offset = (seg.paddr - base) as usize;
            blob[offset..offset + seg.data.len()].copy_from_slice(&seg.data);
        }

        // mem_size: include .bss regions in this place.
        let (place_start, place_end) = place_ranges.iter()
            .find(|(_, name)| *name == place_name)
            .map(|((s, e), _)| (*s, *e))
            .unwrap();

        let mem_end = layout.placed.values()
            .filter(|a| a.base >= place_start && a.base < place_end)
            .map(|a| a.base + a.size)
            .max()
            .unwrap_or(end);

        let mem_size = (mem_end - base) as u32;

        // Compute file_offset.
        let file_offset = if Some(place_name) == host_place_name {
            let host = host_base.ok_or_else(|| LinkError::Other(
                "host place exists in by_place but has no CPU mapping".into()
            ))?;
            let off = (base - host) as u32;
            // Reserve the rest of the host's data region for its own bytes.
            tail_cursor = off + total as u32;
            off
        } else {
            // Pack RAM-init segments after the image data.
            let off = (tail_cursor + 3) & !3;
            tail_cursor = off + total as u32;
            off
        };

        emit(crate::build::BuildEvent::Image(
            crate::build::ResourceUpdate::Event(crate::build::ImageEvent::PlaceWritten {
                place: place_name.to_string(),
                dest: base,
                file_offset,
                file_size: total as u32,
                mem_size,
            }),
        ));

        place_layouts.insert(
            place_name.to_string(),
            PlaceLayout { file_offset, blob_base: base },
        );

        builder.add_segment(base as u32, file_offset, &blob, mem_size);
    }

    let places_bin = builder.build();
    std::fs::write(&places_path, &places_bin).map_err(LinkError::Io)?;

    Ok((places_path, place_layouts))
}

/// Measure the flat-binary size of an ELF (like the length of
/// `objcopy -O binary` output). Used to populate the ftab's
/// `imgs[BL].size` field for the bootloader, which now lives inside
/// places.bin rather than as a separate flashed blob.
pub fn measure_flat_binary_size(artifact: &CompileArtifact) -> Result<u32, LinkError> {
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
    let mut found = false;

    for header in phdrs {
        if header.p_type(endian) != object::elf::PT_LOAD {
            continue;
        }
        let filesz = header.p_filesz(endian) as u64;
        if filesz == 0 {
            continue;
        }
        if (header.p_offset(endian) as usize) < 64 {
            continue;
        }
        let paddr = header.p_paddr(endian) as u64;
        min_addr = min_addr.min(paddr);
        max_addr = max_addr.max(paddr + filesz);
        found = true;
    }

    if !found {
        return Err(LinkError::NoSegments);
    }

    Ok((max_addr - min_addr) as u32)
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
