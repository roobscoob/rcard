use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::{AppConfig, TaskConfig};
use crate::layout::{self, Layout, RegionKey};

/// What role this artifact plays in the firmware image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Task,
    Kernel,
    Bootloader,
}

#[derive(Debug)]
pub struct CompileArtifact {
    pub crate_name: String,
    pub elf_path: PathBuf,
    pub kind: ArtifactKind,
}

/// Three-phase build:
/// 1. Cargo build with partial linking (-r)
/// 2. Re-link for sizing → resolve deferred regions
/// 3. Re-link at final addresses
///
/// Emits `Build(Organizing)` and `Build(CompilingApp)` at the appropriate
/// points so the event stream reflects the real pipeline structure.
pub fn compile_all(
    firmware_dir: &Path,
    config: &AppConfig,
    layout: &mut Layout,
    reservations: &layout::Reservations,
    linker_dir: &Path,
    work_dir: &Path,
    emit: crate::build::EventFn<'_>,
) -> Result<Vec<CompileArtifact>, CompileError> {
    use crate::build::{BuildEvent, BuildState, CrateEvent, CrateKind, CrateState, ResourceUpdate};

    let target = &config.target;
    let all_tasks = layout::collect_tasks(config);

    let task_names: Vec<&str> = layout::ordered_task_names(&all_tasks);
    let hubris_tasks = task_names.join(",");

    let partial_dir = work_dir.join("partial");
    let sizing_dir = work_dir.join("sizing");
    let final_dir = work_dir.join("final");
    std::fs::create_dir_all(&partial_dir).map_err(CompileError::Io)?;
    std::fs::create_dir_all(&sizing_dir).map_err(CompileError::Io)?;
    std::fs::create_dir_all(&final_dir).map_err(CompileError::Io)?;

    let linker = find_linker()?;
    let manifests = resolve_workspace_manifests(firmware_dir)?;
    let config_json = work_dir.join("config.json");

    // Helper to emit a crate state transition.
    let emit_crate = |name: &str, kind: CrateKind, state: CrateState| {
        emit(BuildEvent::Crate {
            name: name.to_string(),
            kind,
            update: ResourceUpdate::State(state),
        });
    };

    // Helper to emit a crate event.
    let emit_crate_event = |name: &str, kind: CrateKind, event: CrateEvent| {
        emit(BuildEvent::Crate {
            name: name.to_string(),
            kind,
            update: ResourceUpdate::Event(event),
        });
    };

    // ── Building: Cargo build with partial linking ────────────────────

    let partial_link_script = write_partial_link_script(work_dir)?;
    let kernel_partial_link_script = write_kernel_partial_link_script(work_dir)?;

    for (crate_name, task) in &all_tasks {
        emit_crate(crate_name, CrateKind::Task, CrateState::Building);
        let manifest = find_manifest(&manifests, crate_name)?;
        cargo_build_partial(
            &manifest,
            crate_name,
            target,
            &partial_link_script,
            work_dir,
            &hubris_tasks,
            &task.features,
            None,
            &config_json,
            crate_name,
            CrateKind::Task,
            emit,
        )?;
        let artifact = firmware_dir
            .join("target")
            .join(target)
            .join("release")
            .join(crate_name);
        if artifact.exists() {
            std::fs::copy(&artifact, partial_dir.join(crate_name)).map_err(CompileError::Io)?;
        }
    }

    // ── Measuring: Re-link for sizing → resolve deferred ─────────────

    let mut measured: BTreeMap<RegionKey, u64> = BTreeMap::new();

    for &task_name in &task_names {
        let partial_obj = partial_dir.join(task_name);
        if !partial_obj.exists() {
            continue;
        }

        emit_crate(task_name, CrateKind::Task, CrateState::Measuring);

        let task = all_tasks[task_name];
        let sizing_elf = sizing_dir.join(task_name);

        let (sizing_memory, chunk_map) = generate_generous_memory_x(task);
        let sizing_mem_dir = sizing_dir.join(format!("{task_name}_mem"));
        std::fs::create_dir_all(&sizing_mem_dir).map_err(CompileError::Io)?;
        std::fs::write(sizing_mem_dir.join("memory.x"), &sizing_memory)
            .map_err(CompileError::Io)?;

        let link_script = linker_dir.join(task_name).join("link.x");
        run_linker(
            &linker,
            &partial_obj,
            &sizing_elf,
            &link_script,
            &sizing_mem_dir,
        )?;

        let sizes = measure_region_sizes(&sizing_elf, &chunk_map)?;
        for (region_name, size) in sizes {
            let key = (task_name.to_string(), region_name.clone());
            if layout.deferred.contains_key(&key) {
                emit_crate_event(task_name, CrateKind::Task, CrateEvent::Sized {
                    region: region_name.clone(),
                    size,
                });
                measured.insert(key, size);
            }
        }
    }

    // Resolve task deferred regions from measurements.
    layout
        .resolve_deferred(&measured, reservations)
        .map_err(|e| CompileError::Other(format!("resolve layout: {e}")))?;

    // ── Organizing ───────────────────────────────────────────────────
    // Signal that the layout has been re-solved with actual sizes.

    emit(BuildEvent::Build(BuildState::Organizing {
        regions_placed: layout.placed.len(),
    }));

    // Emit Memory events for newly placed deferred regions.
    crate::build::emit_memory_allocations(&layout.placed, config, emit);

    // ── Linking: Re-link at final addresses ──────────────────────────

    let mut artifacts = Vec::new();

    for &task_name in &task_names {
        let partial_obj = partial_dir.join(task_name);
        if !partial_obj.exists() {
            continue;
        }

        emit_crate(task_name, CrateKind::Task, CrateState::Linking);

        let task = all_tasks[task_name];
        let final_elf = final_dir.join(task_name);

        let final_memory = generate_final_memory_x(task_name, task, layout);
        let final_mem_dir = work_dir.join("final_mem").join(task_name);
        std::fs::create_dir_all(&final_mem_dir).map_err(CompileError::Io)?;
        std::fs::write(final_mem_dir.join("memory.x"), &final_memory).map_err(CompileError::Io)?;

        let link_script = linker_dir.join(task_name).join("link.x");
        run_linker(
            &linker,
            &partial_obj,
            &final_elf,
            &link_script,
            &final_mem_dir,
        )?;

        emit_crate(task_name, CrateKind::Task, CrateState::Linked);

        artifacts.push(CompileArtifact {
            crate_name: task_name.to_string(),
            elf_path: final_elf,
            kind: ArtifactKind::Task,
        });
    }

    // ── Compile App: Kernel ──────────────────────────────────────────

    emit(BuildEvent::Build(BuildState::CompilingApp));

    let kconfig = generate_kconfig(layout, &task_names, &all_tasks, config)?;

    let kernel_crate = &config.kernel.crate_info.package.name;
    let kernel_manifest = find_manifest(&manifests, kernel_crate)?;
    let kernel_dir = linker_dir.join("kernel");
    let kernel_task = config_to_fake_task(config);

    // Building
    emit_crate(kernel_crate, CrateKind::Kernel, CrateState::Building);
    cargo_build_partial(
        &kernel_manifest,
        kernel_crate,
        target,
        &kernel_partial_link_script,
        work_dir,
        &hubris_tasks,
        &[],
        Some(&kconfig),
        &config_json,
        kernel_crate,
        CrateKind::Kernel,
        emit,
    )?;
    let kernel_partial_src = firmware_dir
        .join("target")
        .join(target)
        .join("release")
        .join(kernel_crate);
    let kernel_partial = partial_dir.join(kernel_crate);
    if kernel_partial_src.exists() {
        std::fs::copy(&kernel_partial_src, &kernel_partial).map_err(CompileError::Io)?;
    }

    // Measuring
    emit_crate(kernel_crate, CrateKind::Kernel, CrateState::Measuring);
    let (kernel_sizing_memory, kernel_chunk_map) = generate_generous_memory_x(&kernel_task);
    let kernel_sizing_dir = sizing_dir.join(format!("{kernel_crate}_mem"));
    std::fs::create_dir_all(&kernel_sizing_dir).map_err(CompileError::Io)?;
    std::fs::write(kernel_sizing_dir.join("memory.x"), &kernel_sizing_memory)
        .map_err(CompileError::Io)?;

    let device_x_src = kernel_dir.join("device.x");
    std::fs::copy(&device_x_src, kernel_sizing_dir.join("device.x")).map_err(CompileError::Io)?;

    let kernel_sizing_elf = sizing_dir.join(kernel_crate);
    run_linker(
        &linker,
        &kernel_partial,
        &kernel_sizing_elf,
        &kernel_dir.join("link.x"),
        &kernel_sizing_dir,
    )?;

    let kernel_sizes = measure_region_sizes(&kernel_sizing_elf, &kernel_chunk_map)?;
    let mut kernel_measured: BTreeMap<RegionKey, u64> = BTreeMap::new();
    for (region_name, size) in &kernel_sizes {
        emit_crate_event(kernel_crate, CrateKind::Kernel, CrateEvent::Sized {
            region: region_name.clone(),
            size: *size,
        });
        kernel_measured.insert(("kernel".to_string(), region_name.clone()), *size);
    }

    layout
        .resolve_kernel_deferred(&kernel_measured, reservations)
        .map_err(|e| CompileError::Other(format!("resolve kernel layout: {e}")))?;

    // Linking
    emit_crate(kernel_crate, CrateKind::Kernel, CrateState::Linking);
    let kernel_memory = generate_final_memory_x("kernel", &kernel_task, layout);
    std::fs::write(kernel_dir.join("memory.x"), &kernel_memory).map_err(CompileError::Io)?;

    let kernel_final_elf = final_dir.join(kernel_crate);
    run_linker(
        &linker,
        &kernel_partial,
        &kernel_final_elf,
        &kernel_dir.join("link.x"),
        &kernel_dir,
    )?;

    emit_crate(kernel_crate, CrateKind::Kernel, CrateState::Linked);

    artifacts.push(CompileArtifact {
        crate_name: kernel_crate.to_string(),
        elf_path: kernel_final_elf,
        kind: ArtifactKind::Kernel,
    });

    // ── Compile App: Bootloader (optional) ───────────────────────────

    if let Some(bl) = &config.bootloader {
        let bl_kconfig = generate_bootloader_kconfig(config)?;
        let bl_crate = &bl.crate_info.package.name;
        let bl_manifest = find_manifest(&manifests, bl_crate)?;
        let bl_linker_dir = linker_dir.join("bootloader");
        let bl_task = bl_to_fake_task(bl);
        let bl_partial_link_script = write_bootloader_partial_link_script(work_dir)?;

        // Building
        emit_crate(bl_crate, CrateKind::Bootloader, CrateState::Building);
        cargo_build_partial(
            &bl_manifest,
            bl_crate,
            target,
            &bl_partial_link_script,
            work_dir,
            &hubris_tasks,
            &[],
            Some(&bl_kconfig),
            &config_json,
            bl_crate,
            CrateKind::Bootloader,
            emit,
        )?;
        let bl_partial_src = firmware_dir
            .join("target")
            .join(target)
            .join("release")
            .join(bl_crate);
        let bl_partial = partial_dir.join(bl_crate);
        if bl_partial_src.exists() {
            std::fs::copy(&bl_partial_src, &bl_partial).map_err(CompileError::Io)?;
        }

        // Measuring
        emit_crate(bl_crate, CrateKind::Bootloader, CrateState::Measuring);
        let (bl_sizing_memory, bl_chunk_map) = generate_generous_memory_x(&bl_task);
        let bl_sizing_dir = sizing_dir.join(format!("{bl_crate}_mem"));
        std::fs::create_dir_all(&bl_sizing_dir).map_err(CompileError::Io)?;
        std::fs::write(bl_sizing_dir.join("memory.x"), &bl_sizing_memory)
            .map_err(CompileError::Io)?;

        let bl_sizing_elf = sizing_dir.join(bl_crate);
        run_linker(
            &linker,
            &bl_partial,
            &bl_sizing_elf,
            &bl_linker_dir.join("link.x"),
            &bl_sizing_dir,
        )?;

        let bl_sizes = measure_region_sizes(&bl_sizing_elf, &bl_chunk_map)?;
        let mut bl_measured: BTreeMap<RegionKey, u64> = BTreeMap::new();
        for (region_name, size) in &bl_sizes {
            emit_crate_event(bl_crate, CrateKind::Bootloader, CrateEvent::Sized {
                region: region_name.clone(),
                size: *size,
            });
            bl_measured.insert(("bootloader".to_string(), region_name.clone()), *size);
        }

        layout
            .resolve_bootloader_deferred(&bl_measured, reservations)
            .map_err(|e| CompileError::Other(format!("resolve bootloader layout: {e}")))?;

        // Linking
        emit_crate(bl_crate, CrateKind::Bootloader, CrateState::Linking);
        let bl_memory = generate_final_memory_x("bootloader", &bl_task, layout);
        std::fs::write(bl_linker_dir.join("memory.x"), &bl_memory).map_err(CompileError::Io)?;

        let bl_final_elf = final_dir.join(bl_crate);
        run_linker(
            &linker,
            &bl_partial,
            &bl_final_elf,
            &bl_linker_dir.join("link.x"),
            &bl_linker_dir,
        )?;

        emit_crate(bl_crate, CrateKind::Bootloader, CrateState::Linked);

        artifacts.push(CompileArtifact {
            crate_name: bl_crate.to_string(),
            elf_path: bl_final_elf,
            kind: ArtifactKind::Bootloader,
        });
    }

    Ok(artifacts)
}

/// Create a fake TaskConfig from bootloader config for memory.x generation.
fn bl_to_fake_task(bl: &crate::config::BootloaderConfig) -> TaskConfig {
    TaskConfig {
        crate_info: bl.crate_info.clone(),
        priority: 0,
        regions: bl.regions.clone(),
        supervisor: false,
        depends_on: vec![],
        peers: vec![],
        uses_peripherals: vec![],
        uses_partitions: vec![],
        pushes_notifications: vec![],
        uses_notifications: vec![],
        features: vec![],
    }
}

/// Generate the bootloader's KCONFIG: firmware partition flash address and size.
fn generate_bootloader_kconfig(config: &AppConfig) -> Result<String, CompileError> {
    // The bootloader needs to know where places.bin lives in flash.
    // Find the image place from config.places.
    let fw_place = config.places.get("image")
        .ok_or_else(|| CompileError::Other("no 'image' place in config".into()))?;
    let fw_offset = fw_place.offset.unwrap_or(0);
    let flash_base = fw_place.mappings.iter()
        .find(|m| m.execute)
        .or_else(|| fw_place.mappings.first())
        .map(|m| m.address)
        .unwrap_or(0x12000000);
    let fw_addr = flash_base + fw_offset;
    let fw_size = fw_place.size;
    Ok(format!("{fw_addr:#010x}_u32,{fw_size:#x}_u32"))
}

fn write_bootloader_partial_link_script(work_dir: &Path) -> Result<PathBuf, CompileError> {
    let path = work_dir.join("bootloader-rlink.x");
    std::fs::write(
        &path,
        r#"ENTRY(_start);
SECTIONS
{
  .text : { *(.text.start*); *(.text .text.*); . = ALIGN(4); }
  .rodata : ALIGN(4) { *(.rodata .rodata.*); . = ALIGN(4); }
  /DISCARD/ : { *(.ARM.exidx); *(.ARM.exidx.*); *(.ARM.extab.*); *(.got .got.*); *(.data .data.*); *(.bss .bss.*); }
}
"#,
    )
    .map_err(CompileError::Io)?;
    Ok(path)
}

/// Create a fake TaskConfig from kernel config for memory.x generation.
fn config_to_fake_task(config: &AppConfig) -> TaskConfig {
    TaskConfig {
        crate_info: config.kernel.crate_info.clone(),
        priority: 0,
        regions: config.kernel.regions.clone(),
        supervisor: false,
        depends_on: vec![],
        peers: vec![],
        uses_peripherals: vec![],
        uses_partitions: vec![],
        pushes_notifications: vec![],
        uses_notifications: vec![],
        features: vec![],
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn find_linker() -> Result<PathBuf, CompileError> {
    let output = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .map_err(CompileError::Io)?;
    let sysroot = String::from_utf8_lossy(&output.stdout).trim().to_string();
    for host in [
        "x86_64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu",
        "aarch64-apple-darwin",
    ] {
        let lld = PathBuf::from(&sysroot)
            .join("lib")
            .join("rustlib")
            .join(host)
            .join("bin")
            .join("rust-lld");
        if lld.exists() {
            return Ok(lld);
        }
        let lld_exe = lld.with_extension("exe");
        if lld_exe.exists() {
            return Ok(lld_exe);
        }
    }
    Ok(PathBuf::from("rust-lld"))
}

fn run_linker(
    linker: &Path,
    input: &Path,
    output: &Path,
    link_script: &Path,
    search_dir: &Path,
) -> Result<(), CompileError> {
    let result = std::process::Command::new(linker)
        .arg("-flavor")
        .arg("gnu")
        .arg(input)
        .arg("-o")
        .arg(output)
        .arg(format!("-T{}", link_script.display()))
        .arg(format!("-L{}", search_dir.display()))
        .arg("--gc-sections")
        .args(["-m", "armelf"])
        .args(["-z", "common-page-size=0x20"])
        .args(["-z", "max-page-size=0x20"])
        .output()
        .map_err(CompileError::Io)?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(CompileError::Linker {
            message: format!("{}: {stderr}", output.display()),
        });
    }
    Ok(())
}

fn write_partial_link_script(work_dir: &Path) -> Result<PathBuf, CompileError> {
    let path = work_dir.join("task-rlink.x");
    std::fs::write(
        &path,
        r#"ENTRY(_start);
SECTIONS
{
  .text : { _stext = .; *(.text.start*); *(.text .text.*); . = ALIGN(4); __etext = .; }
  .rodata : ALIGN(4) { *(.rodata .rodata.*); . = ALIGN(4); __erodata = .; }
  .data : ALIGN(4) { . = ALIGN(4); __sdata = .; *(.data .data.*); . = ALIGN(4); __edata = .; }
  __sidata = LOADADDR(.data);
  .bss (NOLOAD) : ALIGN(4) { . = ALIGN(4); __sbss = .; *(.bss .bss.*); . = ALIGN(4); __ebss = .; }
  .uninit (NOLOAD) : ALIGN(4) { . = ALIGN(4); *(.uninit .uninit.*); . = ALIGN(4); __sheap = .; }
  .task_slot_table (INFO) : { KEEP(*(.task_slot_table)); }
  .caboose_pos_table (INFO) : { KEEP(*(.caboose_pos_table)); }
  .hubris_abi_version (INFO) : { KEEP(*(.hubris_abi_version)); }
  .idolatry (INFO) : { KEEP(*(.idolatry)); }
  .log_strings (INFO) : { KEEP(*(.log_strings .log_strings.*)); }
  /DISCARD/ : { *(.ARM.exidx); *(.ARM.exidx.*); *(.ARM.extab.*); *(.got .got.*); }
}
"#,
    )
    .map_err(CompileError::Io)?;
    Ok(path)
}

fn write_kernel_partial_link_script(work_dir: &Path) -> Result<PathBuf, CompileError> {
    let path = work_dir.join("kernel-rlink.x");
    std::fs::write(
        &path,
        r#"ENTRY(Reset);

/* Pull in cortex-m-rt symbols that are only referenced by the
   final kernel link script, not by code. Without these the
   relocatable link would drop them from the archive. */
EXTERN(__RESET_VECTOR);
EXTERN(__EXCEPTIONS);
EXTERN(DefaultHandler);
EXTERN(DefaultHandler_);
EXTERN(HardFaultTrampoline);
EXTERN(DefaultPreInit);
EXTERN(__INTERRUPTS);

SECTIONS
{
  /* Keep vector table sections separate so the final kernel link
     script can place them in .vector_table. */
  .vector_table.reset_vector : { KEEP(*(.vector_table.reset_vector)); }
  .vector_table.exceptions   : { KEEP(*(.vector_table.exceptions)); }
  .vector_table.interrupts   : { KEEP(*(.vector_table.interrupts)); }

  .text : {
    *(.PreResetTrampoline);
    *(.Reset);
    *(.text .text.*);
    *(.HardFaultTrampoline);
    *(.HardFault.*);
    . = ALIGN(4);
  }
  .rodata : ALIGN(4) { *(.rodata .rodata.*); . = ALIGN(4); }
  .data : ALIGN(4) { *(.data .data.*); . = ALIGN(4); }
  .gnu.sgstubs : { *(.gnu.sgstubs*); }
  .bss (NOLOAD) : ALIGN(4) { *(.bss .bss.*); *(COMMON); . = ALIGN(4); }
  .uninit (NOLOAD) : ALIGN(4) { *(.uninit .uninit.*); . = ALIGN(4); }
  .log_strings (INFO) : { KEEP(*(.log_strings .log_strings.*)); }
  /DISCARD/ : { *(.ARM.exidx); *(.ARM.exidx.*); *(.ARM.extab.*); *(.got .got.*); }
}
"#,
    )
    .map_err(CompileError::Io)?;
    Ok(path)
}

fn cargo_build_partial(
    manifest: &Path,
    crate_name: &str,
    target: &str,
    linker_script: &Path,
    work_dir: &Path,
    hubris_tasks: &str,
    features: &[String],
    hubris_kconfig: Option<&str>,
    config_json: &Path,
    emit_crate_name: &str,
    emit_crate_kind: crate::build::CrateKind,
    emit: crate::build::EventFn<'_>,
) -> Result<(), CompileError> {
    use crate::build::{BuildEvent, CrateEvent, ResourceUpdate};

    let dir = work_dir.display().to_string().replace('\\', "/");
    let script = linker_script.display().to_string().replace('\\', "/");
    let flags = [
        format!("-Clink-arg=-L{dir}"),
        format!("-Clink-arg=-T{script}"),
        "-Clink-arg=-r".to_string(),
    ];
    let mut build = escargot::CargoBuild::new()
        .manifest_path(manifest)
        .target(target)
        .release()
        .env("HUBRIS_TASKS", hubris_tasks)
        .env("TFW_CONFIG_JSON", config_json.display().to_string())
        .env("CARGO_ENCODED_RUSTFLAGS", flags.join("\x1f"))
        .env_remove("RUSTFLAGS");
    if let Some(kconfig) = hubris_kconfig {
        build = build
            .env("HUBRIS_KCONFIG", kconfig)
            .env("HUBRIS_IMAGE_ID", "0");
    }
    if !features.is_empty() {
        build = build.features(features.join(","));
    }
    let messages = build
        .exec()
        .map_err(|e| CompileError::Other(format!("failed to run cargo for {crate_name}: {e:#}")))?;
    let mut failed = false;
    for msg in messages {
        match msg {
            Ok(msg) => emit(BuildEvent::Crate {
                name: emit_crate_name.to_string(),
                kind: emit_crate_kind,
                update: ResourceUpdate::Event(CrateEvent::CargoMessage(msg)),
            }),
            Err(e) => {
                emit(BuildEvent::Crate {
                    name: emit_crate_name.to_string(),
                    kind: emit_crate_kind,
                    update: ResourceUpdate::Event(CrateEvent::CargoError(e)),
                });
                failed = true;
            }
        }
    }
    if failed {
        return Err(CompileError::Cargo {
            crate_name: crate_name.to_string(),
        });
    }
    Ok(())
}

/// For sizing: give each region a large non-overlapping chunk.
/// Returns (memory.x content, map of chunk_base_address → region_name).
fn generate_generous_memory_x(task: &TaskConfig) -> (String, BTreeMap<u64, String>) {
    use std::fmt::Write;
    let mut out = String::from("MEMORY\n{\n");
    let mut chunk_map: BTreeMap<u64, String> = BTreeMap::new();
    let mut base: u64 = 0x01000000;
    let chunk: u64 = 0x01000000;
    for (region_name, req) in &task.regions {
        let linker_name = match region_name.as_str() {
            "code" => "FLASH",
            "data" => "RAM",
            "stack" => "STACK",
            other => &other.to_uppercase(),
        };
        let size = req.size.unwrap_or(chunk);
        let attrs = if region_name == "code" { "rx" } else { "rw" };
        writeln!(
            out,
            "  {linker_name} ({attrs}) : ORIGIN = {base:#010x}, LENGTH = {size:#x}"
        )
        .unwrap();
        chunk_map.insert(base, region_name.clone());
        base += chunk;
    }
    out.push_str("}\n");
    (out, chunk_map)
}

/// For final link: all regions are now in layout.placed.
fn generate_final_memory_x(task_name: &str, task: &TaskConfig, layout: &Layout) -> String {
    use std::fmt::Write;
    let mut out = String::from("MEMORY\n{\n");

    for (region_name, _req) in &task.regions {
        let key = (task_name.to_string(), region_name.clone());
        let linker_name = match region_name.as_str() {
            "code" => "FLASH",
            "data" => "RAM",
            "stack" => "STACK",
            other => &other.to_uppercase(),
        };
        let attrs = if region_name == "code" { "rx" } else { "rw" };

        if let Some(alloc) = layout.placed.get(&key) {
            writeln!(
                out,
                "  {linker_name} ({attrs}) : ORIGIN = {:#010x}, LENGTH = {:#x}",
                alloc.base, alloc.size
            )
            .unwrap();
        }
    }

    // Kernel needs VECTORS = same as FLASH
    if task_name == "kernel" {
        if let Some(alloc) = layout
            .placed
            .get(&("kernel".to_string(), "code".to_string()))
        {
            writeln!(
                out,
                "  VECTORS (rx) : ORIGIN = {:#010x}, LENGTH = {:#x}",
                alloc.base, alloc.size
            )
            .unwrap();
        }
    }

    out.push_str("}\n");
    out
}

/// Measure region sizes from a linked ELF.
///
/// `region_bases` maps region base addresses to region names. Each PT_LOAD
/// segment is attributed to the region whose base is closest (<=) to the
/// segment's address. This works for both generous memory layouts (where
/// regions are spaced far apart) and real layouts (where regions may be
/// close together), as long as the base addresses are correct.
fn measure_region_sizes(
    elf_path: &Path,
    region_bases: &BTreeMap<u64, String>,
) -> Result<BTreeMap<String, u64>, CompileError> {
    use object::read::elf::{ElfFile32, FileHeader, ProgramHeader};

    let data = std::fs::read(elf_path).map_err(CompileError::Io)?;
    let elf = ElfFile32::<object::Endianness>::parse(&*data)
        .map_err(|e| CompileError::Other(format!("ELF parse: {e}")))?;
    let endian = elf.endian();
    let phdrs = elf
        .elf_header()
        .program_headers(endian, elf.data())
        .map_err(|e| CompileError::Other(format!("phdrs: {e}")))?;

    // Sorted bases for lookup
    let bases: Vec<(u64, &str)> = region_bases
        .iter()
        .map(|(base, name)| (*base, name.as_str()))
        .collect();

    // Find which region an address belongs to (largest base <= addr)
    let find_region = |addr: u64| -> Option<&str> {
        bases
            .iter()
            .rev()
            .find(|(base, _)| *base <= addr)
            .map(|(_, name)| *name)
    };

    // Track (min_addr, max_addr) per region name
    let mut extents: BTreeMap<&str, (u64, u64)> = BTreeMap::new();

    for header in phdrs {
        if header.p_type(endian) != object::elf::PT_LOAD {
            continue;
        }
        let memsz = header.p_memsz(endian) as u64;
        let filesz = header.p_filesz(endian) as u64;
        if memsz == 0 && filesz == 0 {
            continue;
        }

        let vaddr = header.p_vaddr(endian) as u64;
        let paddr = header.p_paddr(endian) as u64;

        // VMA region (where it lives in memory)
        if let Some(region) = find_region(vaddr) {
            let entry = extents.entry(region).or_insert((u64::MAX, 0));
            entry.0 = entry.0.min(vaddr);
            entry.1 = entry.1.max(vaddr + memsz);
        }

        // LMA region (where it's loaded from — for .data in flash)
        if paddr != vaddr && filesz > 0 {
            if let Some(region) = find_region(paddr) {
                let entry = extents.entry(region).or_insert((u64::MAX, 0));
                entry.0 = entry.0.min(paddr);
                entry.1 = entry.1.max(paddr + filesz);
            }
        }
    }

    let mut result = BTreeMap::new();
    for (region_name, (min_addr, max_addr)) in &extents {
        let size = ((max_addr - min_addr) + 31) & !31; // align up to 32
        result.insert(region_name.to_string(), size);
    }

    Ok(result)
}

/// Scheduling priorities: position in `task_names` (which is already in
/// canonical order from `layout::ordered_task_names`).
fn compute_priorities(task_names: &[&str]) -> BTreeMap<String, u8> {
    task_names
        .iter()
        .enumerate()
        .map(|(i, &name)| (name.to_string(), i as u8))
        .collect()
}

/// Generate KCONFIG from unified layout — all regions are placed.
fn generate_kconfig(
    layout: &Layout,
    task_names: &[&str],
    all_tasks: &BTreeMap<&str, &TaskConfig>,
    config: &AppConfig,
) -> Result<String, CompileError> {
    let priorities = compute_priorities(task_names);

    let mut kconfig = build_kconfig::KernelConfig {
        features: vec!["stack_watermark".to_string()],
        extern_regions: BTreeMap::new(),
        tasks: Vec::new(),
        shared_regions: BTreeMap::new(),
        irqs: BTreeMap::new(),
    };

    // Register peripherals as shared regions (Device memory).
    for (name, periph) in &config.peripheral_map {
        kconfig.shared_regions.insert(
            name.clone(),
            build_kconfig::RegionConfig {
                base: periph.base as u32,
                size: periph.size as u32,
                attributes: build_kconfig::RegionAttributes {
                    read: true,
                    write: true,
                    execute: false,
                    special_role: Some(build_kconfig::SpecialRole::Device),
                },
            },
        );
    }

    for (task_index, &task_name) in task_names.iter().enumerate() {
        let task = all_tasks[task_name];
        let mut owned_regions = BTreeMap::new();

        // All regions come from layout.placed — no flash/ram distinction.
        // Skip zero-size allocations: they exist only for linker script
        // ORIGIN and have no content to protect. On ARMv8-M, a zero-size
        // MPU region would cause an RLAR wraparound covering all memory.
        for ((owner, region_name), alloc) in &layout.placed {
            if owner != task_name {
                continue;
            }
            if alloc.size == 0 {
                continue;
            }
            let (read, write, execute) = match region_name.as_str() {
                "code" => (true, false, true),
                _ => (true, true, false),
            };
            owned_regions.insert(
                region_name.clone(),
                build_kconfig::MultiRegionConfig {
                    base: alloc.base as u32,
                    sizes: vec![alloc.size as u32],
                    attributes: build_kconfig::RegionAttributes {
                        read,
                        write,
                        execute,
                        special_role: None,
                    },
                },
            );
        }

        // Grant access to peripherals declared in the task config.
        let mut task_shared = BTreeSet::new();
        for periph_name in &task.uses_peripherals {
            task_shared.insert(periph_name.clone());

            // Route peripheral IRQs to this task.
            if let Some(periph) = config.peripheral_map.get(periph_name) {
                for (_irq_name, &irq_num) in &periph.irqs {
                    kconfig.irqs.insert(
                        irq_num,
                        build_kconfig::InterruptConfig {
                            task_index,
                            notification: 1 << (irq_num % 32),
                        },
                    );
                }
            }
        }

        let stack_size = layout
            .placed
            .get(&(task_name.to_string(), "stack".to_string()))
            .map(|a| a.size as u32)
            .unwrap_or(0);

        kconfig.tasks.push(build_kconfig::TaskConfig {
            owned_regions,
            shared_regions: task_shared,
            entry_point: build_kconfig::OwnedAddress {
                region_name: "code".to_string(),
                offset: 0,
            },
            initial_stack: build_kconfig::OwnedAddress {
                region_name: "stack".to_string(),
                offset: stack_size,
            },
            priority: priorities.get(task_name).copied().unwrap_or(0),
            start_at_boot: true,
        });
    }

    ron::ser::to_string(&kconfig).map_err(CompileError::KconfigSerialize)
}

/// Resolve a workspace crate name to its Cargo.toml path using `cargo metadata`.
/// Caches the result so we only invoke cargo once per build.
fn resolve_workspace_manifests(
    firmware_dir: &Path,
) -> Result<BTreeMap<String, PathBuf>, CompileError> {
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version=1"])
        .current_dir(firmware_dir)
        .output()
        .map_err(CompileError::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CompileError::Other(format!(
            "cargo metadata failed: {stderr}"
        )));
    }

    let meta: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| CompileError::Other(format!("parse cargo metadata: {e}")))?;

    let mut manifests = BTreeMap::new();
    if let Some(packages) = meta["packages"].as_array() {
        for pkg in packages {
            if let (Some(name), Some(manifest)) =
                (pkg["name"].as_str(), pkg["manifest_path"].as_str())
            {
                manifests.insert(name.to_string(), PathBuf::from(manifest));
            }
        }
    }

    Ok(manifests)
}

fn find_manifest(
    manifests: &BTreeMap<String, PathBuf>,
    crate_name: &str,
) -> Result<PathBuf, CompileError> {
    manifests
        .get(crate_name)
        .cloned()
        .ok_or_else(|| CompileError::ManifestNotFound {
            crate_name: crate_name.to_string(),
        })
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot find Cargo.toml for {crate_name}")]
    ManifestNotFound { crate_name: String },
    #[error("cargo build failed for {crate_name}")]
    Cargo { crate_name: String },
    #[error("linker error: {message}")]
    Linker { message: String },
    #[error("no artifact produced for {crate_name}")]
    NoArtifact { crate_name: String },
    #[error("failed to serialize KCONFIG: {0}")]
    KconfigSerialize(ron::Error),
    #[error("{0}")]
    Other(String),
}
