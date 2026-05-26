use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use crate::codegen::CodegenError;
use crate::compile::CompileError;
use crate::config::{AppConfig, ConfigError};
use crate::ipc_metadata::IpcMetadataError;
use crate::layout::LayoutError;
use crate::link::LinkError;
use crate::linker::LinkerError;
use crate::log_metadata::MetadataError;
use crate::pack::PackError;

// ── Resource system ────────────────────────────────────────────────────────
//
// Build progress is modeled as a set of **resources**, each with its own
// state machine.  Every update is self-contained: it names the resource
// that changed, and whether it transitioned to a new **state** (durable —
// the resource *is* this) or an **event** occurred (transient — something
// *happened*).
//
// Resources:
//   Build      — the overall pipeline (singleton)
//   Crate      — a firmware crate compiled for the embedded target
//   HostCrate  — a crate compiled and run on the host (e.g. schema dumper)
//   Memory     — a named memory place on the board
//   Image      — the output firmware image (singleton)

/// A resource defines its own state and event types.
pub trait Resource: 'static {
    /// Durable state — the resource *is* this.
    type State: std::fmt::Debug;
    /// Transient event — something *happened* to this resource.
    type Event: std::fmt::Debug;
}

/// An update to a resource: either a state transition or a transient event.
#[derive(Debug)]
pub enum ResourceUpdate<R: Resource> {
    /// The resource transitioned to a new state.
    State(R::State),
    /// A transient event occurred while in the current state.
    Event(R::Event),
}

// ── Build resource (singleton) ─────────────────────────────────────────────

/// The overall build pipeline.
pub struct Build;

impl Resource for Build {
    type State = BuildState;
    type Event = ();
}

/// Major phases of the build pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildState {
    /// Evaluating Nickel config, solving initial layout,
    /// generating linker scripts and codegen source.
    Planning,
    /// Building all task crates through build/measure/link passes.
    CompilingTasks,
    /// All task regions measured; re-solving the memory layout
    /// with actual sizes.
    Organizing { regions_placed: usize },
    /// Building kernel and bootloader crates.
    CompilingApp,
    /// Scraping log/IPC metadata from ELFs, running the schema dumper.
    ExtractingMetadata,
    /// Assembling the firmware image and writing the archive.
    Packing,
    /// Build finished successfully.
    Done,
}

// ── Crate resource (firmware crate) ────────────────────────────────────────

/// What role a firmware crate plays in the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrateKind {
    Task,
    Kernel,
    Bootloader,
}

/// A firmware crate compiled for the embedded target.
#[derive(Debug, Clone)]
pub struct Crate {
    pub name: String,
    pub kind: CrateKind,
}

impl Resource for Crate {
    type State = CrateState;
    type Event = CrateEvent;
}

/// Durable states — what pass this crate is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrateState {
    /// Cargo is building this crate into a relocatable object.
    Building,
    /// Cargo has finished producing the relocatable object; waiting
    /// for the batched Measuring pass to start. This exists so the
    /// UI can stop showing "building…" the moment a crate is actually
    /// done compiling, even when sibling crates are still in Cargo.
    Compiled,
    /// The relocatable object is being linked at temporary addresses
    /// to measure how much memory its code and data actually need.
    Measuring,
    /// The crate is being linked at its final memory addresses.
    Linking,
    /// Final ELF produced, ready for image assembly.
    Linked,
}

/// Transient events — things that happen while in a state.
#[derive(Debug)]
pub enum CrateEvent {
    /// A memory region was measured during the Measuring state.
    Sized { region: String, size: u64 },
    /// Cargo emitted a compiler message (warning, error, etc).
    CargoMessage(escargot::Message),
    /// Cargo failed while building this crate.
    CargoError(escargot::error::CargoError),
}

// ── HostCrate resource ─────────────────────────────────────────────────────

/// A crate compiled and run on the host (e.g. the schema dumper).
/// Different lifecycle from firmware crates — no sizing or
/// address-specific linking.
#[derive(Debug, Clone)]

pub struct HostCrate {
    pub name: String,
}

impl Resource for HostCrate {
    type State = HostCrateState;
    type Event = HostCrateEvent;
}

/// Durable states for a host-target crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCrateState {
    /// Waiting to start — visible in the crate list but greyed out.
    Queued,
    /// Being compiled for the host target.
    Building,
    /// Compiled, now executing.
    Running,
    /// Finished executing.
    Done,
}

/// Transient events for a host-target crate.
#[derive(Debug)]
pub enum HostCrateEvent {
    /// Cargo emitted a compiler message.
    CargoMessage(escargot::Message),
    /// Cargo failed.
    CargoError(escargot::error::CargoError),
}

// ── Memory resource ────────────────────────────────────────────────────────

/// A named memory place on the board (e.g. `"sram_fast_dctm"`, `"image"`).
/// Place names come from `config.places` — the layout names assigned in
/// `.ncl` files. The resource identity is where the allocation landed.
#[derive(Debug, Clone)]
pub struct Memory {
    pub place: String,
}

/// No states — unit type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoState;

impl Resource for Memory {
    type State = NoState;
    type Event = MemoryEvent;
}

/// Transient events on a memory place.
#[derive(Debug, Clone)]
pub enum MemoryEvent {
    /// A region was allocated in this memory place.
    Allocated {
        /// Crate name, `"kernel"`, or `"bootloader"`.
        owner: String,
        /// Region name: `"code"`, `"data"`, `"stack"`, etc.
        region: String,
        /// CPU address where the region was placed.
        base: u64,
        /// Size in bytes.
        size: u64,
        /// Context about the original request.
        request: AllocationRequest,
    },
}

/// Context about the original region request that led to an allocation.
#[derive(Debug, Clone)]
pub struct AllocationRequest {
    /// Place name where the region was requested to go.
    /// When this differs from the Memory resource's place name,
    /// the allocation overflowed to an alternative.
    pub requested_place: String,
    /// Size from the request, if specified. `None` = sized by linker (deferred).
    pub requested_size: Option<u64>,
    /// Alignment constraint in bytes.
    pub align: Option<u64>,
    /// Sharing group name, if any.
    pub shared: Option<String>,
}

// ── Image resource (singleton) ─────────────────────────────────────────────

/// The output firmware image being assembled.
#[derive(Debug, Clone)]
pub struct Image;

impl Resource for Image {
    type State = ImageState;
    type Event = ImageEvent;
}

/// Durable states for the firmware image.
#[derive(Debug, Clone)]
pub enum ImageState {
    /// The flat binary image (`places.bin`) was assembled.
    Assembled { size: u64 },
    /// The `.tfw` archive was written to disk.
    Archived { path: PathBuf },
}

/// Transient events during image assembly.
#[derive(Debug, Clone)]
pub enum ImageEvent {
    /// A memory place was written into the output image.
    PlaceWritten {
        place: String,
        dest: u64,
        file_offset: u32,
        file_size: u32,
        mem_size: u32,
    },
}

// ── Type-erased event stream ───────────────────────────────────────────────

/// Static information that falls out of config load at the start of
/// Planning — UUID, physical memory devices, named place capacities.
/// Carried as a single one-shot bundle rather than three discrete
/// events, because none of this data changes during the build.
#[derive(Debug, Clone)]
pub struct ResolvedLayout {
    pub build_id: String,
    /// Resolved app name from the Nickel config (e.g. "rcard").
    pub name: String,
    pub memories: Vec<ResolvedMemoryDevice>,
    /// `(place_name, size_bytes)` for every named place.
    pub places: Vec<(String, u64)>,
    /// Per-task metadata from the config tree — priority, kind,
    /// dependency edges. Lets the UI show task info before compile
    /// events arrive.
    pub tasks: Vec<ResolvedTaskInfo>,
}

/// Static per-task metadata from the config. Available immediately
/// after config load in the Planning phase.
#[derive(Debug, Clone)]
pub struct ResolvedTaskInfo {
    pub name: String,
    pub kind: CrateKind,
    pub priority: u32,
    /// Names of tasks this task directly depends on.
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedMemoryDevice {
    pub name: String,
    pub size: u64,
    /// `(cpu_address, size)` pairs for each distinct CPU mapping.
    pub mappings: Vec<(u64, u64)>,
}

/// Concrete event type sent through the callback channel.
/// Wraps each resource's updates into a single enum for type erasure.
#[derive(Debug)]
pub enum BuildEvent {
    /// The overall build pipeline changed state.
    Build(BuildState),
    /// One-shot delivery of the resolved config: build id, memory
    /// devices, place capacities. Fired once during Planning, right
    /// after config load. Bundles data that used to be three separate
    /// events — none of this changes across the rest of the build.
    ConfigResolved(ResolvedLayout),
    /// Full IPC metadata bundle produced by the ExtractingMetadata
    /// stage. Emitted once, after `ipc_metadata::scrape` and task-id
    /// population. Lets the UI populate its Resources card for a live
    /// build — previously only loaded firmware had this data.
    IpcMetadata(crate::ipc_metadata::IpcMetadataBundle),
    /// A firmware crate was updated.
    Crate {
        name: String,
        kind: CrateKind,
        update: ResourceUpdate<Crate>,
    },
    /// A host-target crate was updated.
    HostCrate {
        name: String,
        update: ResourceUpdate<HostCrate>,
    },
    /// A memory place received an allocation.
    Memory {
        place: String,
        update: ResourceUpdate<Memory>,
    },
    /// The output image was updated.
    Image(ResourceUpdate<Image>),
}

/// Callback for build events.
pub type EventFn<'a> = &'a dyn Fn(BuildEvent);

fn noop(_: BuildEvent) {}

// ── Build pipeline ──────────────────────────────────────────────────────────

/// Full build pipeline: plan → compile tasks (shared) → per-image
/// build (solve + link + kernel) → extract metadata → pack.
pub fn build(
    firmware_dir: &Path,
    root_ncl: &str,
    board_ncl: &str,
    images: &[crate::config::ImageSpec],
    out_path: &Path,
    on_event: Option<EventFn<'_>>,
    work_dir: Option<&Path>,
) -> Result<PathBuf, BuildError> {
    // Wrap the user's event sink so we can quietly tee every solved
    // memory allocation into a local collector, and time the whole
    // build for duration persistence. These two things end up in the
    // archive at pack time so the GUI can show accurate utilisation +
    // timing for any loaded firmware.
    let user_emit: EventFn<'_> = on_event.unwrap_or(&noop);
    let build_started = std::time::Instant::now();
    let collected_allocs: std::cell::RefCell<Vec<crate::build_metadata::AllocationRecord>> =
        std::cell::RefCell::new(Vec::new());
    let collected_messages: std::cell::RefCell<Vec<crate::build_metadata::CargoMessageRecord>> =
        std::cell::RefCell::new(Vec::new());
    let emit_closure = |event: BuildEvent| {
        match &event {
            BuildEvent::Memory {
                place,
                update:
                    ResourceUpdate::Event(MemoryEvent::Allocated {
                        owner,
                        region,
                        base,
                        size,
                        request,
                    }),
            } => {
                collected_allocs
                    .borrow_mut()
                    .push(crate::build_metadata::AllocationRecord {
                        place: place.clone(),
                        owner: owner.clone(),
                        region: region.clone(),
                        base: *base,
                        size: *size,
                        requested_place: request.requested_place.clone(),
                    });
            }
            BuildEvent::Crate {
                name,
                update: ResourceUpdate::Event(CrateEvent::CargoMessage(msg)),
                ..
            } => {
                if let Ok(val) = msg.decode_custom::<serde_json::Value>() {
                    if let Ok(raw) = serde_json::to_string(&val) {
                        collected_messages
                            .borrow_mut()
                            .push(crate::build_metadata::CargoMessageRecord {
                                crate_name: name.clone(),
                                raw,
                            });
                    }
                }
            }
            BuildEvent::HostCrate {
                name,
                update: ResourceUpdate::Event(HostCrateEvent::CargoMessage(msg)),
            } => {
                if let Ok(val) = msg.decode_custom::<serde_json::Value>() {
                    if let Ok(raw) = serde_json::to_string(&val) {
                        collected_messages
                            .borrow_mut()
                            .push(crate::build_metadata::CargoMessageRecord {
                                crate_name: name.clone(),
                                raw,
                            });
                    }
                }
            }
            _ => {}
        }
        user_emit(event);
    };
    let emit: EventFn<'_> = &emit_closure;

    let _tmp;
    let work_dir = match work_dir {
        Some(p) => p.to_path_buf(),
        None => {
            _tmp = tempfile::tempdir()
                .map_err(|e| BuildError::Compile(crate::compile::CompileError::Io(e)))?;
            _tmp.path().to_path_buf()
        }
    };
    let linker_dir = work_dir.join("linker");
    let img_dir = work_dir.join("img");
    let log_metadata_path = work_dir.join("log-metadata.json");
    let ipc_metadata_path = work_dir.join("ipc-metadata.json");
    let config_json_path = work_dir.join("config.json");

    // ── Plan ───────────────────────────────────────────────────────────
    // Evaluate Nickel config per image, using the first image's config
    // as the canonical one for shared phases.

    emit(BuildEvent::Build(BuildState::Planning));

    assert!(!images.is_empty(), "at least one image must be specified");

    let mut image_configs: Vec<(crate::config::ImageSpec, crate::config::AppConfig)> =
        Vec::with_capacity(images.len());
    for img in images {
        let cfg = crate::config::load(firmware_dir, root_ncl, board_ncl, &img.layout_ncl)
            .map_err(BuildError::Config)?;
        image_configs.push((img.clone(), cfg));
    }

    let config = &image_configs[0].1;

    // Generate the build's identity up-front so it can be bundled
    // with the resolved-layout broadcast.
    let build_id = uuid::Uuid::new_v4().to_string();

    // One-shot: everything about the resolved config the UI needs
    // to render its static chrome (memory map devices, place
    // capacities, build id). None of this changes during the build.
    // Walk the config task tree to collect per-task metadata for the
    // UI (priority, dependency edges). Same tree shape as the snapshot
    // path uses; doing it here means live builds have task info from
    // the very start of the pipeline.
    let mut resolved_tasks = Vec::new();
    {
        let mut seen = std::collections::HashSet::new();
        fn walk_config_tasks(
            task: &crate::config::TaskConfig,
            out: &mut Vec<ResolvedTaskInfo>,
            seen: &mut std::collections::HashSet<String>,
        ) {
            let name = &task.crate_info.package.name;
            if !seen.insert(name.clone()) {
                return;
            }
            out.push(ResolvedTaskInfo {
                name: name.clone(),
                kind: CrateKind::Task,
                priority: task.priority,
                depends_on: task
                    .depends_on
                    .iter()
                    .map(|d| d.crate_info.package.name.clone())
                    .collect(),
            });
            for dep in &task.depends_on {
                walk_config_tasks(dep, out, seen);
            }
        }
        // Add kernel + bootloader as well.
        resolved_tasks.push(ResolvedTaskInfo {
            name: config.kernel.crate_info.package.name.clone(),
            kind: CrateKind::Kernel,
            priority: 0,
            depends_on: Vec::new(),
        });
        if let Some(bl) = &config.bootloader {
            resolved_tasks.push(ResolvedTaskInfo {
                name: bl.crate_info.package.name.clone(),
                kind: CrateKind::Bootloader,
                priority: 0,
                depends_on: Vec::new(),
            });
        }
        for task in &config.entries {
            walk_config_tasks(task, &mut resolved_tasks, &mut seen);
        }
    }

    emit(BuildEvent::ConfigResolved(ResolvedLayout {
        build_id: build_id.clone(),
        name: config.name.clone(),
        memories: config
            .memory
            .iter()
            .map(|(name, mem)| ResolvedMemoryDevice {
                name: name.clone(),
                size: mem.size,
                mappings: mem
                    .mappings
                    .iter()
                    .map(|m| (m.address, m.size))
                    .collect(),
            })
            .collect(),
        places: config
            .places
            .iter()
            .map(|(name, place)| (name.clone(), place.size))
            .collect(),
        tasks: resolved_tasks,
    }));

    // Compute reservations from all images' code_generic places.
    let image_code_places: Vec<&crate::config::Place> = image_configs.iter()
        .filter_map(|(_, cfg)| cfg.places.get("code_generic"))
        .collect();
    let reservations = crate::layout::compute_reservations(&image_code_places);

    // Use the first image's config for initial layout solve, linker
    // script generation, and codegen. These are used for the shared
    // task compilation phase.
    let initial_layout = crate::layout::solve(config, &reservations).map_err(BuildError::Layout)?;
    emit_memory_allocations(&initial_layout.placed, config, emit);

    crate::linker::generate(config, &initial_layout, &linker_dir).map_err(BuildError::Linker)?;
    crate::codegen::emit(config, &build_id, &config_json_path).map_err(BuildError::Codegen)?;

    // ── Compile tasks (shared) ────────────────────────────────────────
    // Build all task partial objects and measure sizes. This phase is
    // layout-independent — partial objects and sizes are reused across
    // all image builds.

    emit(BuildEvent::Build(BuildState::CompilingTasks));

    let shared = crate::compile::compile_tasks_shared(
        firmware_dir,
        config,
        &linker_dir,
        &work_dir,
        emit,
    )
    .map_err(BuildError::Compile)?;

    // ── Per-image builds ──────────────────────────────────────────────
    // For each image: solve layout, link tasks, build kernel with
    // image-specific KCONFIG, produce artifacts.

    let mut image_builds: Vec<crate::compile::ImageBuild> = Vec::new();

    for (img_spec, img_config) in &image_configs {
        let mut img_layout = crate::layout::solve(img_config, &reservations)
            .map_err(BuildError::Layout)?;

        let artifacts = crate::compile::build_image(
            &img_spec.name,
            firmware_dir,
            img_config,
            &mut img_layout,
            &reservations,
            &shared,
            &linker_dir,
            &work_dir,
            emit,
        )
        .map_err(BuildError::Compile)?;

        image_builds.push(crate::compile::ImageBuild {
            name: img_spec.name.clone(),
            layout: img_layout,
            config: img_config.clone(),
            artifacts,
        });
    }

    // Use first image's artifacts for metadata extraction.
    let artifacts = &image_builds[0].artifacts;

    // ── Extract metadata ───────────────────────────────────────────────
    // Scrape log/IPC metadata from ELFs, run the schema dumper.

    emit(BuildEvent::Build(BuildState::ExtractingMetadata));

    let task_names: Vec<String> = artifacts
        .iter()
        .filter(|a| a.kind == crate::compile::ArtifactKind::Task)
        .map(|a| a.crate_name.clone())
        .collect();
    let log_bundle =
        crate::log_metadata::scrape(&task_names, &artifacts).map_err(BuildError::Metadata)?;
    crate::log_metadata::emit(&log_bundle, &log_metadata_path).map_err(BuildError::Metadata)?;

    let mut ipc_bundle =
        crate::ipc_metadata::scrape(&artifacts).map_err(BuildError::IpcMetadata)?;

    let generated_config = crate::codegen::build_config(&config, &build_id);
    for server in ipc_bundle.servers.values_mut() {
        if let Some(&idx) = generated_config.task_indices.get(&server.task) {
            server.task_id = Some(idx as u16);
        }
    }
    // Announce the full bundle now that `task_id` fields are set —
    // the UI needs it to populate the Resources card for live builds.
    emit(BuildEvent::IpcMetadata(ipc_bundle.clone()));

    // Schema dump — compile a host binary to extract postcard-schema types.
    let api_crates: Vec<crate::schema_dump::ApiCrate> = ipc_bundle
        .resources
        .values()
        .filter_map(|r| {
            let crate_path = r.crate_path.as_ref()?;
            let crate_name = r.crate_name.as_ref()?;
            Some(crate::schema_dump::ApiCrate {
                package: crate_name.clone(),
                path: std::path::PathBuf::from(crate_path),
                resource_names: vec![r.name.clone()],
            })
        })
        .collect();

    let mut deduped: std::collections::BTreeMap<String, crate::schema_dump::ApiCrate> =
        std::collections::BTreeMap::new();
    for api in api_crates {
        deduped
            .entry(api.package.clone())
            .and_modify(|existing| {
                existing.resource_names.extend(api.resource_names.clone());
            })
            .or_insert(api);
    }
    let api_crates: Vec<_> = deduped.into_values().collect();

    if !api_crates.is_empty() {
        emit(BuildEvent::HostCrate {
            name: "ipc-schema-dumper".into(),
            update: ResourceUpdate::State(HostCrateState::Queued),
        });
        match crate::schema_dump::run(&api_crates, &work_dir, emit) {
            Ok(schema_json) => {
                if let Ok(schemas) = serde_json::from_str::<serde_json::Value>(&schema_json) {
                    ipc_bundle.schemas = Some(schemas);
                }
            }
            Err(e) => {
                return Err(BuildError::SchemaDump(e));
            }
        }
    }

    crate::ipc_metadata::emit(&ipc_bundle, &ipc_metadata_path).map_err(BuildError::IpcMetadata)?;

    // ── Pack ───────────────────────────────────────────────────────────
    // Link each image's artifacts into a places.bin, then assemble the
    // archive with ftab and metadata.

    emit(BuildEvent::Build(BuildState::Packing));

    let mut linked_images: Vec<(String, std::path::PathBuf, BTreeMap<String, crate::link::PlaceLayout>)> = Vec::new();

    for (i, ib) in image_builds.iter().enumerate() {
        let filename = if i == 0 {
            "places.bin".to_string()
        } else {
            format!("places_{}.bin", ib.name)
        };
        let code_place = ib.config.places.get("code_generic");
        let (bin_path, place_layouts) = crate::link::link_image(
            &ib.artifacts, &ib.config, &ib.layout, &img_dir,
            &filename, code_place, emit,
        ).map_err(BuildError::Link)?;

        let bin_size = std::fs::metadata(&bin_path).map(|m| m.len()).unwrap_or(0);
        emit(BuildEvent::Image(ResourceUpdate::State(
            ImageState::Assembled { size: bin_size },
        )));

        linked_images.push((ib.name.clone(), bin_path, place_layouts));
    }

    let layout_ncl_display = images.iter()
        .map(|i| i.layout_ncl.as_str())
        .collect::<Vec<_>>()
        .join(",");

    let mut build_meta = crate::build_metadata::BuildMetadata::from_build(
        &build_id,
        &config.name,
        root_ncl,
        config.version.as_deref(),
        board_ncl,
        &layout_ncl_display,
        firmware_dir,
    );
    build_meta.build_duration_ms = Some(build_started.elapsed().as_millis() as u64);
    build_meta.allocations = collected_allocs.borrow().clone();
    build_meta.cargo_messages = collected_messages.borrow().clone();

    // Build image info for pack: (spec, config, bin_path, place_layouts, artifacts)
    let pack_images: Vec<_> = images.iter()
        .zip(image_configs.iter())
        .zip(linked_images.iter())
        .zip(image_builds.iter())
        .map(|(((spec, (_, cfg)), (_, bin_path, place_layouts)), ib)| {
            (spec, cfg, bin_path.as_path(), place_layouts, ib.artifacts.as_slice())
        })
        .collect();

    // Compute flash-info metadata for ftab construction at flash time.
    let flash_info = compute_flash_info(
        config,
        &image_configs,
        &image_builds,
        &linked_images,
    );

    crate::pack::pack(
        config,
        &pack_images,
        flash_info.as_ref(),
        Some(&log_metadata_path),
        Some(&ipc_metadata_path),
        Some(&build_meta),
        out_path,
    )
    .map_err(BuildError::Pack)?;

    emit(BuildEvent::Image(ResourceUpdate::State(
        ImageState::Archived {
            path: out_path.to_path_buf(),
        },
    )));
    emit(BuildEvent::Build(BuildState::Done));
    Ok(out_path.to_path_buf())
}

// ── Memory event helpers ──────────────────────────────────────────────

/// Look up the `RegionRequest` for a given (owner, region) pair from the config.
fn find_region_request<'a>(
    config: &'a AppConfig,
    owner: &str,
    region: &str,
) -> Option<&'a crate::config::RegionRequest> {
    if owner == "kernel" {
        return config.kernel.regions.get(region);
    }
    if owner == "bootloader" {
        if let Some(bl) = &config.bootloader {
            return bl.regions.get(region);
        }
        return None;
    }
    // Walk task tree.
    fn find_in_tasks<'a>(
        entries: &'a [crate::config::TaskConfig],
        owner: &str,
        region: &str,
    ) -> Option<&'a crate::config::RegionRequest> {
        for task in entries {
            if task.crate_info.package.name == owner {
                return task.regions.get(region);
            }
            if let Some(req) = find_in_tasks(&task.depends_on, owner, region) {
                return Some(req);
            }
        }
        None
    }
    find_in_tasks(&config.entries, owner, region)
}

/// Emit Memory::Allocated events for all entries in `placed`.
pub fn emit_memory_allocations(
    placed: &BTreeMap<(String, String), crate::layout::Allocation>,
    config: &AppConfig,
    emit: EventFn<'_>,
) {
    for ((owner, region), alloc) in placed {
        if alloc.size == 0 {
            continue;
        }
        let actual_place = crate::layout::find_place_name(config, alloc.base)
            .unwrap_or_else(|| "unknown".into());

        let request = if let Some(req) = find_region_request(config, owner, region) {
            AllocationRequest {
                requested_place: req
                    .place
                    .as_ref()
                    .and_then(|p| p.name.clone())
                    .unwrap_or_else(|| actual_place.clone()),
                requested_size: req.size,
                align: req.align,
                shared: req.shared.clone(),
            }
        } else {
            AllocationRequest {
                requested_place: actual_place.clone(),
                requested_size: None,
                align: None,
                shared: None,
            }
        };

        emit(BuildEvent::Memory {
            place: actual_place,
            update: ResourceUpdate::Event(MemoryEvent::Allocated {
                owner: owner.clone(),
                region: region.clone(),
                base: alloc.base,
                size: alloc.size,
                request,
            }),
        });
    }
}

// ── Flash-info computation ──────────────────────────────────────────────────

/// Compute the flash metadata needed for ftab construction at flash time.
/// Returns `None` if no boot config exists (RAM-only builds).
fn compute_flash_info(
    config: &AppConfig,
    image_configs: &[(crate::config::ImageSpec, AppConfig)],
    image_builds: &[crate::compile::ImageBuild],
    linked_images: &[(String, std::path::PathBuf, BTreeMap<String, crate::link::PlaceLayout>)],
) -> Option<crate::pack::FlashInfo> {
    let boot = config.boot.as_ref()?;

    let ftab_place = &boot.ftab;
    let ftab_offset = ftab_place.offset.unwrap_or(0);
    let flash_base = ftab_place
        .mappings
        .first()
        .map(|m| m.address)
        .unwrap_or(0x12000000) as u32;
    let ftab_base = flash_base + ftab_offset as u32;

    // Bootloader location within places.bin: use the first image's layout.
    let first_build = &image_builds[0];
    let bootloader_alloc = first_build.layout.placed
        .get(&("bootloader".to_string(), "code".to_string()))?;
    let loader_sram_dest = bootloader_alloc.base as u32;

    let first_config = &image_configs[0].1;
    let first_code_place = first_config.places.get("code_generic")?;
    let first_flash_base = first_code_place
        .mappings
        .first()
        .map(|m| m.address)
        .unwrap_or(0x12000000) as u32;
    let first_flash_addr = first_flash_base + first_code_place.offset.unwrap_or(0) as u32;

    let (_, _, ref first_place_layouts) = linked_images[0];
    let bl_place_name = crate::layout::find_place_name(first_config, bootloader_alloc.base)?;
    let pl = first_place_layouts.get(&bl_place_name)?;
    let bl_flash_src = (first_flash_addr as u64
        + pl.file_offset as u64
        + (bootloader_alloc.base - pl.blob_base)) as u32;
    let bl_file_offset = bl_flash_src - first_flash_addr;

    let bl_artifact = first_build.artifacts.iter()
        .find(|a| a.kind == crate::compile::ArtifactKind::Bootloader)?;
    let bl_size = crate::link::measure_flat_binary_size(bl_artifact).ok()?;

    let mut image_infos = Vec::new();
    for (i, (_spec, img_config)) in image_configs.iter().enumerate() {
        let code_place = img_config.places.get("code_generic")?;
        let img_flash_base = code_place
            .mappings
            .first()
            .map(|m| m.address)
            .unwrap_or(0x12000000) as u32;
        let img_flash_addr = img_flash_base + code_place.offset.unwrap_or(0) as u32;
        image_infos.push(crate::pack::ImageFlashInfo {
            name: linked_images[i].0.clone(),
            flash_addr: img_flash_addr,
            ftab_slot: crate::pack::FTAB_PLACES_SLOT_A + i,
        });
    }

    Some(crate::pack::FlashInfo {
        ftab_base,
        bootloader_file_offset: bl_file_offset,
        bootloader_sram_dest: loader_sram_dest,
        bootloader_size: bl_size,
        images: image_infos,
    })
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
    #[error("schema dump: {0}")]
    SchemaDump(#[from] crate::schema_dump::SchemaDumpError),
    #[error("link: {0}")]
    Link(#[from] LinkError),
    #[error("pack: {0}")]
    Pack(#[from] PackError),
}
