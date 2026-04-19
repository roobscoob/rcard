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
    /// Whether this region is shared with other tasks.
    pub shared: bool,
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

/// Full build pipeline: plan → compile tasks → organize → compile app →
/// extract metadata → pack.
pub fn build(
    firmware_dir: &Path,
    root_ncl: &str,
    board_ncl: &str,
    layout_ncl: &str,
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
    // Evaluate Nickel config, solve initial layout, generate linker
    // scripts and codegen source.

    emit(BuildEvent::Build(BuildState::Planning));

    let config = crate::config::load(firmware_dir, root_ncl, board_ncl, layout_ncl)
        .map_err(BuildError::Config)?;

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

    let reservations = crate::layout::compute_reservations(&config);
    let mut layout = crate::layout::solve(&config, &reservations).map_err(BuildError::Layout)?;

    // Emit Memory events for fixed-size allocations placed during initial solve.
    emit_memory_allocations(&layout.placed, &config, emit);

    crate::linker::generate(&config, &layout, &linker_dir).map_err(BuildError::Linker)?;

    crate::codegen::emit(&config, &build_id, &config_json_path).map_err(BuildError::Codegen)?;

    // ── Compile tasks ──────────────────────────────────────────────────
    // Build all task crates through build/measure/link passes.

    emit(BuildEvent::Build(BuildState::CompilingTasks));

    let mut artifacts = crate::compile::compile_all(
        firmware_dir,
        &config,
        &mut layout,
        &reservations,
        &linker_dir,
        &work_dir,
        emit,
    )
    .map_err(BuildError::Compile)?;

    // Organizing and CompilingApp events are emitted inside compile_all
    // at the correct pipeline boundaries.

    // Separate bootloader artifact from firmware artifacts.
    let bl_artifact_idx = artifacts
        .iter()
        .position(|a| a.kind == crate::compile::ArtifactKind::Bootloader);
    let bl_artifact = bl_artifact_idx.map(|i| artifacts.remove(i));

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
            update: ResourceUpdate::State(HostCrateState::Building),
        });
        match crate::schema_dump::run(&api_crates, &work_dir) {
            Ok(schema_json) => {
                emit(BuildEvent::HostCrate {
                    name: "ipc-schema-dumper".into(),
                    update: ResourceUpdate::State(HostCrateState::Running),
                });
                if let Ok(schemas) = serde_json::from_str::<serde_json::Value>(&schema_json) {
                    ipc_bundle.schemas = Some(schemas);
                }
                emit(BuildEvent::HostCrate {
                    name: "ipc-schema-dumper".into(),
                    update: ResourceUpdate::State(HostCrateState::Done),
                });
            }
            Err(e) => {
                return Err(BuildError::SchemaDump(e));
            }
        }
    }

    crate::ipc_metadata::emit(&ipc_bundle, &ipc_metadata_path).map_err(BuildError::IpcMetadata)?;

    // ── Pack ───────────────────────────────────────────────────────────
    // Assemble the firmware image and write the archive.

    emit(BuildEvent::Build(BuildState::Packing));

    let final_bin = crate::link::link_image(&artifacts, &config, &layout, &img_dir, emit)
        .map_err(BuildError::Link)?;

    let bootloader_size = if let Some(ref bl_art) = bl_artifact {
        crate::link::measure_flat_binary_size(bl_art).map_err(BuildError::Link)?
    } else {
        0
    };

    // Re-add bootloader for ELF packing.
    if let Some(bl_art) = bl_artifact {
        artifacts.push(bl_art);
    }

    let bin_size = std::fs::metadata(&final_bin).map(|m| m.len()).unwrap_or(0);
    emit(BuildEvent::Image(ResourceUpdate::State(
        ImageState::Assembled { size: bin_size },
    )));

    let mut build_meta = crate::build_metadata::BuildMetadata::from_build(
        &build_id,
        &config.name,
        root_ncl,
        config.version.as_deref(),
        board_ncl,
        layout_ncl,
        firmware_dir,
    );
    // Stamp duration + solved allocations so tooling reading this .tfw
    // later can render a faithful memory map and timing without having
    // to re-solve.
    build_meta.build_duration_ms = Some(build_started.elapsed().as_millis() as u64);
    build_meta.allocations = collected_allocs.borrow().clone();
    build_meta.cargo_messages = collected_messages.borrow().clone();
    crate::pack::pack(
        &config,
        &layout,
        &artifacts,
        &final_bin,
        bootloader_size,
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

/// Find the place name from `config.places` that contains the given CPU address.
fn find_place_name(config: &AppConfig, addr: u64) -> Option<String> {
    for (name, place) in &config.places {
        if place.unmapped || place.mappings.is_empty() {
            continue;
        }
        let offset = place.offset.unwrap_or(0);
        for mapping in &place.mappings {
            let start = mapping.address + offset;
            let end = start + place.size;
            if addr >= start && addr < end {
                return Some(name.clone());
            }
        }
    }
    None
}

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
        let actual_place =
            find_place_name(config, alloc.base).unwrap_or_else(|| "unknown".into());

        let request = if let Some(req) = find_region_request(config, owner, region) {
            AllocationRequest {
                requested_place: req
                    .place
                    .name
                    .clone()
                    .unwrap_or_else(|| actual_place.clone()),
                requested_size: req.size,
                align: req.align,
                shared: req.shared,
            }
        } else {
            AllocationRequest {
                requested_place: actual_place.clone(),
                requested_size: None,
                align: None,
                shared: false,
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
