use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use crate::codegen::CodegenError;
use crate::compile::CompileError;
use crate::config::{AppConfig, ConfigError};
use crate::ipc_metadata::IpcMetadataError;
use crate::layout::{Layout, LayoutError};
use crate::link::LinkError;
use crate::linker::LinkerError;
use crate::log_metadata::MetadataError;
use crate::pack::PackError;

// ── Build events ────────────────────────────────────────────────────────────

/// Top-level build stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Config,
    Layout,
    Linker,
    Codegen,
    Compile,
    LogMetadata,
    IpcMetadata,
    Link,
    Pack,
}

/// Compile sub-phase within the Compile stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompilePhase {
    PartialLink,
    Sizing,
    FinalLink,
    Kernel,
    Bootloader,
}

/// A segment in the memory map visualization.
#[derive(Debug, Clone)]
pub struct MemSegment {
    /// Memory region name (e.g. "flash", "ram").
    pub memory: String,
    /// Owner task name, or "(unused)" for free space.
    pub owner: String,
    /// Region name within the owner (e.g. "text", "data"). None for unused.
    pub region: Option<String>,
    /// CPU base address.
    pub base: u64,
    /// Size in bytes.
    pub size: u64,
    /// Alignment waste (gap before this segment).
    pub lost: u64,
}

/// Structured event emitted during a build.
/// Consumers can pattern-match to render progress however they want.
#[derive(Debug)]
pub enum BuildEvent {
    /// A top-level stage is starting.
    StageStart { stage: Stage },

    /// A compile sub-phase is starting.
    PhaseStart { phase: CompilePhase },

    /// A task is being compiled in the given phase.
    TaskCompiling { task: String, phase: CompilePhase },

    /// A deferred region was measured during the sizing phase.
    RegionMeasured { task: String, region: String, size: u64 },

    /// All deferred regions have been resolved into the layout.
    LayoutResolved { total_regions: usize },

    /// Memory map segments, ready for visualization.
    MemoryMap { segments: Vec<MemSegment> },

    /// A raw cargo message. Call `.decode()` for structured access.
    CargoMessage(escargot::Message),

    /// The flat binary was produced.
    ImageLinked { size: u64 },

    /// The firmware archive was written.
    Packed { path: PathBuf },

    /// The build completed successfully.
    Done,
}

/// Callback for structured build events.
pub type EventFn<'a> = &'a dyn Fn(BuildEvent);

fn noop(_: BuildEvent) {}

// ── Build pipeline ──────────────────────────────────────────────────────────

/// Full build pipeline: config → layout → linker → codegen → compile → link → pack.
pub fn build(
    firmware_dir: &Path,
    root_ncl: &str,
    board_ncl: &str,
    layout_ncl: &str,
    out_path: &Path,
    on_event: Option<EventFn<'_>>,
    work_dir: Option<&Path>,
) -> Result<PathBuf, BuildError> {
    let emit = on_event.unwrap_or(&noop);

    let _tmp;
    let work_dir = match work_dir {
        Some(p) => p.to_path_buf(),
        None => {
            _tmp = tempfile::tempdir().map_err(|e| BuildError::Compile(
                crate::compile::CompileError::Io(e),
            ))?;
            _tmp.path().to_path_buf()
        }
    };
    let linker_dir = work_dir.join("linker");
    let img_dir = work_dir.join("img");
    let meta_dir = work_dir.join("log_meta");
    let log_metadata_path = work_dir.join("log-metadata.json");
    let ipc_metadata_path = work_dir.join("ipc-metadata.json");
    let config_json_path = work_dir.join("config.json");

    // 1. Config
    emit(BuildEvent::StageStart { stage: Stage::Config });
    let config = crate::config::load(firmware_dir, root_ncl, board_ncl, layout_ncl)
        .map_err(BuildError::Config)?;

    // 2. Layout
    emit(BuildEvent::StageStart { stage: Stage::Layout });
    let mut layout = crate::layout::solve(&config)
        .map_err(BuildError::Layout)?;

    // 3. Linker scripts
    emit(BuildEvent::StageStart { stage: Stage::Linker });
    crate::linker::generate(&config, &layout, &linker_dir)
        .map_err(BuildError::Linker)?;

    // Generate build ID once — shared between codegen and archive.
    let build_id = uuid::Uuid::new_v4().to_string();

    // 4. Codegen
    emit(BuildEvent::StageStart { stage: Stage::Codegen });
    crate::codegen::emit(&config, &build_id, &config_json_path)
        .map_err(BuildError::Codegen)?;

    // 5. Compile
    emit(BuildEvent::StageStart { stage: Stage::Compile });
    let mut artifacts = crate::compile::compile_all(
        firmware_dir, &config, &mut layout, &linker_dir, &work_dir, emit,
    ).map_err(BuildError::Compile)?;

    // 5b. Memory map
    emit(BuildEvent::MemoryMap { segments: compute_segments(&config, &layout) });

    // Separate bootloader artifact from firmware artifacts.
    let bl_artifact_idx = artifacts.iter()
        .position(|a| a.kind == crate::compile::ArtifactKind::Bootloader);
    let bl_artifact = bl_artifact_idx.map(|i| artifacts.remove(i));

    // 6. Log metadata
    emit(BuildEvent::StageStart { stage: Stage::LogMetadata });
    let task_names: Vec<String> = artifacts
        .iter()
        .filter(|a| a.kind == crate::compile::ArtifactKind::Task)
        .map(|a| a.crate_name.clone())
        .collect();
    let log_bundle = crate::log_metadata::scrape(&task_names, &artifacts)
        .map_err(BuildError::Metadata)?;
    crate::log_metadata::emit(&log_bundle, &log_metadata_path)
        .map_err(BuildError::Metadata)?;

    // 6b. IPC metadata — scrape `.ipc_meta` sections from task ELFs.
    emit(BuildEvent::StageStart { stage: Stage::IpcMetadata });
    let ipc_bundle = crate::ipc_metadata::scrape(&artifacts)
        .map_err(BuildError::IpcMetadata)?;
    crate::ipc_metadata::emit(&ipc_bundle, &ipc_metadata_path)
        .map_err(BuildError::IpcMetadata)?;

    // 7. Link
    emit(BuildEvent::StageStart { stage: Stage::Link });
    let final_bin = crate::link::link_image(&artifacts, &config, &layout, &img_dir)
        .map_err(BuildError::Link)?;

    let bootloader_bin = if let Some(ref bl_art) = bl_artifact {
        Some(crate::link::extract_flat_binary(bl_art, &img_dir)
            .map_err(BuildError::Link)?)
    } else {
        None
    };

    // Re-add bootloader for ELF packing.
    if let Some(bl_art) = bl_artifact {
        artifacts.push(bl_art);
    }

    let bin_size = std::fs::metadata(&final_bin)
        .map(|m| m.len())
        .unwrap_or(0);
    emit(BuildEvent::ImageLinked { size: bin_size });

    // 8. Pack
    emit(BuildEvent::StageStart { stage: Stage::Pack });
    let build_meta = crate::build_metadata::BuildMetadata::from_build(
        &build_id,
        &config.name,
        config.version.as_deref(),
        board_ncl,
        layout_ncl,
        firmware_dir,
    );
    crate::pack::pack(
        &config, &artifacts, &final_bin,
        bootloader_bin.as_deref(),
        Some(&log_metadata_path),
        Some(&ipc_metadata_path),
        Some(&build_meta),
        out_path,
    ).map_err(BuildError::Pack)?;

    emit(BuildEvent::Packed { path: out_path.to_path_buf() });
    emit(BuildEvent::Done);
    Ok(out_path.to_path_buf())
}

// ── Memory map ─────────────────────────────────────────────────────────

/// Build memory map segments from the finalized layout and config.
/// Groups allocations by place, computes alignment gaps, and adds
/// "(unused)" segments for remaining free space.
fn compute_segments(config: &AppConfig, layout: &Layout) -> Vec<MemSegment> {
    let mut segments = Vec::new();

    // Deduplicate places that map to the same CPU address range so we don't
    // print the same physical region multiple times.
    let mut seen_ranges: BTreeMap<(u64, u64), String> = BTreeMap::new();

    for (place_name, place) in &config.places {
        if place.unmapped || place.mappings.is_empty() {
            continue;
        }
        let cpu_base = match crate::layout::resolve_cpu_address(place, false) {
            Some(addr) => addr,
            None => continue,
        };
        let cpu_end = cpu_base + place.size;

        // Skip if we already rendered this exact address range.
        if let std::collections::btree_map::Entry::Vacant(e) = seen_ranges.entry((cpu_base, cpu_end)) {
            e.insert(place_name.clone());
        } else {
            continue;
        }

        // Collect allocations that fall within this place.
        let mut allocs: Vec<(&str, &str, &crate::layout::Allocation)> = layout
            .placed
            .iter()
            .filter(|(_, a)| a.base >= cpu_base && a.base < cpu_end)
            .map(|((owner, region), a)| (owner.as_str(), region.as_str(), a))
            .collect();
        allocs.sort_by_key(|(_, _, a)| a.base);

        let mut cursor = cpu_base;
        for (owner, region, alloc) in &allocs {
            let lost = alloc.base - cursor;
            segments.push(MemSegment {
                memory: place_name.clone(),
                owner: owner.to_string(),
                region: Some(region.to_string()),
                base: alloc.base,
                size: alloc.size,
                lost,
            });
            cursor = alloc.base + alloc.size;
        }

        // Unused tail.
        let remaining = cpu_end.saturating_sub(cursor);
        if remaining > 0 {
            segments.push(MemSegment {
                memory: place_name.clone(),
                owner: "(unused)".to_string(),
                region: None,
                base: cursor,
                size: remaining,
                lost: 0,
            });
        }
    }

    segments
}

// ── Error types ─────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("config: {0}")]
    Config(#[from] ConfigError),
    #[error("layout: {0}")]
    Layout(#[from] LayoutError),
    #[error("linker: {0}")]
    Linker(#[from] LinkerError),
    #[error("codegen: {0}")]
    Codegen(#[from] CodegenError),
    #[error("compile: {0}")]
    Compile(#[from] CompileError),
    #[error("metadata: {0}")]
    Metadata(#[from] MetadataError),
    #[error("ipc metadata: {0}")]
    IpcMetadata(#[from] IpcMetadataError),
    #[error("link: {0}")]
    Link(#[from] LinkError),
    #[error("pack: {0}")]
    Pack(#[from] PackError),
}
