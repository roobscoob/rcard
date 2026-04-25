use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use device::adapter::Adapter;
use device::device::Device;
use device::logs::Log;
use tokio::sync::mpsc;

use crate::state::{
    BuildId, ChipUid, CrateBuildState, CrateKind, DeviceId, DeviceKind, DevicePhase,
    HostCrateBuildState, ImageProgress, KnownCapability, MemoryAllocation,
    MemoryDevice, PipelinePhase, ProvidedResource, ResourceSummary, SerialAdapterType,
    SerialPortIndex, SerialPortStatus, UsedResource,
};

// ── Commands (GUI → bridge) ────────────────────────────────────────────

pub enum Command {
    /// Launch an emulator from a .tfw archive.
    RunEmulator {
        firmware_id: crate::state::FirmwareId,
        tfw_path: PathBuf,
    },
    /// Stop and remove a device (emulator or otherwise).
    RemoveDevice(DeviceId),
    /// Register a serial port for background connection.
    RegisterSerial {
        index: SerialPortIndex,
        port: String,
        adapter_type: SerialAdapterType,
    },
    /// Unregister a serial port.
    UnregisterSerial { index: SerialPortIndex },
    /// Begin flashing firmware to a device via SifliDebug.
    /// Sets a pending flash flag — the actual write happens on next bootrom entry.
    FlashViaSifliDebug {
        device_id: DeviceId,
        firmware_id: crate::state::FirmwareId,
        tfw_path: PathBuf,
    },
    /// Trigger a firmware build.
    Build {
        build_id: BuildId,
        firmware_dir: PathBuf,
        config: String,
        board: String,
        layout: String,
        out: PathBuf,
    },
    /// Make an IPC call to a device.
    IpcCall {
        device_id: DeviceId,
        call: ipc_runtime::EncodedCall,
        /// oneshot to return the result to the caller.
        reply: tokio::sync::oneshot::Sender<Result<crate::ipc::IpcCallResult, String>>,
    },
    /// Fire a one-shot MoshiMoshi probe on the device's USART2 wire.
    ///
    /// Manual diagnostic trigger used while validating the USART1 hello
    /// path: lets the user re-fire MoshiMoshi without restarting the app
    /// (the automatic MoshiMoshi only runs on `Usart2::connect`).
    SendMoshiMoshi { device_id: DeviceId },
    /// Fire a MoshiMoshi probe on *every* bridge device that has a
    /// `SerialSender` (USART2) capability.
    ///
    /// Self-enqueued by `usart1_connect_loop` when the last settling
    /// USART1 transitions into its main read loop — the probe coaxes
    /// each device's supervisor into emitting the `hello` line on the
    /// USART1 wire that now has a ready listener. See the `usart1_settling`
    /// counter in `run()` for the transition logic.
    ProbeMoshiMoshi,
    /// IPC partition writes finished successfully — remove from pending_flash.
    FlashComplete { device_id: DeviceId },
    /// IPC partition writes (or pre-IPC setup) failed — remove from pending_flash
    /// and surface the error in the UI.
    FlashFailed { device_id: DeviceId, message: String },
    /// IPC flash phase progress — forwarded as Event::FlashProgress.
    FlashPhaseUpdate { device_id: DeviceId, phase: FlashPhase },
}

// ── Events (bridge → GUI) ──────────────────────────────────────────────

pub enum Event {
    /// An adapter was created by a connection.
    AdapterCreated {
        adapter_id: device::adapter::AdapterId,
        display_name: String,
        /// Transport label + priority for adapters that contribute the
        /// IPC capability — matches the values the bridge's
        /// `crate::ipc::pick` ranks against. `None` for non-IPC adapters
        /// (SifliDebug-over-USART1, etc.).
        ipc_transport: Option<(&'static str, u8)>,
    },
    /// An adapter was removed.
    AdapterRemoved {
        adapter_id: device::adapter::AdapterId,
    },
    /// An adapter is now bound to a device, contributing capabilities.
    /// The adapter and device must both already exist (via `AdapterCreated`
    /// and `DeviceCreated`).
    AdapterBound {
        adapter_id: device::adapter::AdapterId,
        device_id: DeviceId,
        capabilities: Vec<KnownCapability>,
    },
    /// An adapter is no longer bound to its device. The adapter itself
    /// still exists — it has not been removed.
    AdapterUnbound {
        adapter_id: device::adapter::AdapterId,
        device_id: DeviceId,
    },
    /// A new device entity was created. Adapters are bound separately
    /// via `AdapterBound`.
    DeviceCreated {
        device_id: DeviceId,
        name: String,
        kind: DeviceKind,
        /// The firmware this device is running, if known.
        firmware_id: Option<crate::state::FirmwareId>,
    },
    /// A device was removed.
    DeviceDeleted { device_id: DeviceId },
    /// A log from a device.
    Log { device: DeviceId, log: Log },
    /// Pipeline advanced to a new major phase (Planning, CompilingTasks, …).
    BuildPhase {
        build_id: BuildId,
        phase: PipelinePhase,
    },
    /// One-shot delivery of the resolved config — UUID, memory
    /// devices, place capacities. Arrives once during Planning, after
    /// config load. Replaces the old `BuildUuid` +
    /// `BuildPlaceCapacity` + `BuildMemoryDevice` trio; those were
    /// shaped like events but were really static config data.
    BuildConfigResolved {
        build_id: BuildId,
        uuid: String,
        /// Resolved app name from the Nickel config.
        name: String,
        memories: Vec<MemoryDevice>,
        place_capacities: Vec<(String, u64)>,
        /// Per-task metadata from the config: priority, kind,
        /// dependency edges. Used to pre-populate `CrateProgress`
        /// entries before compile events start arriving.
        tasks: Vec<tfw::build::ResolvedTaskInfo>,
    },
    /// IPC resource list resolved during the ExtractingMetadata stage.
    /// Replaces `BuildHandle::resources` so the live-build view can
    /// render the Resources card instead of leaving it empty until
    /// the archive is loaded from disk.
    BuildResources {
        build_id: BuildId,
        resources: Vec<ResourceSummary>,
        /// Raw IPC bundle for per-crate provides/uses derivation in
        /// state.rs. `resources` above is the card-level summary;
        /// this carries the full server map needed to fill in
        /// individual crate rows.
        bundle: tfw::ipc_metadata::IpcMetadataBundle,
    },
    /// An embedded crate's build state transitioned.
    BuildCrateState {
        build_id: BuildId,
        name: String,
        kind: CrateKind,
        state: CrateBuildState,
    },
    /// An embedded crate reported a measured region size.
    BuildCrateSized {
        build_id: BuildId,
        name: String,
        kind: CrateKind,
        region: String,
        size: u64,
    },
    /// A raw cargo JSON message scoped to a specific crate. The `line`
    /// field is one ndjson line from cargo's `--message-format=json`
    /// output. The frontend decodes it for rendering.
    BuildCrateCargoLine {
        build_id: BuildId,
        name: String,
        kind: CrateKind,
        line: String,
    },
    /// Host-side crate (schema_dump, metadata scrapers) state transition.
    BuildHostCrateState {
        build_id: BuildId,
        name: String,
        state: HostCrateBuildState,
    },
    /// A memory allocation was resolved by the layout solver.
    BuildAllocation {
        build_id: BuildId,
        allocation: MemoryAllocation,
    },
    /// Output image state changed.
    BuildImage {
        build_id: BuildId,
        image: ImageProgress,
    },
    /// Free-form pipeline log line (stage events, unparsed lines).
    BuildLog { build_id: BuildId, message: String },
    /// Build finished.
    BuildComplete {
        build_id: BuildId,
        result: Result<PathBuf, String>,
    },
    /// Device phase changed (bootrom, bootloader, kernel).
    DevicePhaseChanged {
        device_id: DeviceId,
        phase: DevicePhase,
    },
    /// An ephemeral device was identified and upgraded to a persistent one.
    /// Behaves like DeviceDeleted(old_id) + DeviceCreated(new_id), but the
    /// GUI can migrate tiles/focus from old to new.
    DeviceUpgraded { old_id: DeviceId, new_id: DeviceId },
    /// Flash phase changed — drives the flash modal UI.
    FlashProgress {
        device_id: DeviceId,
        phase: FlashPhase,
    },
    /// Serial port status changed.
    SerialStatus {
        index: SerialPortIndex,
        status: SerialPortStatus,
    },
    /// A line of raw text was read from a serial port (USART1).
    SerialRawLine {
        index: SerialPortIndex,
        line: String,
    },
    /// A structured log entry was decoded on a serial port (USART2).
    SerialStructuredLog {
        index: SerialPortIndex,
        entry: device::logs::LogEntry,
    },
    /// A non-log control event (IPC reply, tunnel error) was decoded on a
    /// serial port (USART2).
    SerialControlEvent {
        index: SerialPortIndex,
        event: device::logs::ControlEvent,
    },
    /// A device self-reported its firmware build id (via the Awake
    /// simple-frame on USART2). The state handler parses the bytes as
    /// a UUIDv4 and, if a matching `.tfw` archive is loaded, sets
    /// `DeviceHandle.firmware_id` so the log viewer can resolve
    /// species / type metadata.
    DeviceReportedBuildId {
        device_id: DeviceId,
        build_id_bytes: [u8; 16],
    },
}

/// Phase of a flash operation, driven by the bridge.
///
/// Step indices for `Failed::at_step`:
///   0 = Resetting, 1 = WritingStub, 2 = VerifyingStub, 3 = BootingStub,
///   4 = Erasing, 5 = Programming, 6 = Verifying
pub enum FlashPhase {
    /// Step 0: Attempting to reset the device via SifliDebug.
    Resetting,
    /// Step 0: Auto-reset failed — asking the user to reset manually.
    WaitingForReset,
    /// Step 1: Writing stub firmware to RAM.
    WritingStub {
        bytes_written: u32,
        bytes_total: u32,
    },
    /// Step 2: Verifying stub was written correctly.
    VerifyingStub {
        bytes_verified: u32,
        bytes_total: u32,
    },
    /// Step 3: Stub loaded, device is booting.
    BootingStub,
    /// Step 3: Stub is running and USB-enumerated.
    StubBooted,
    /// Step 4: Erasing flash partition(s).
    Erasing,
    /// Step 5: Programming firmware to flash.
    Programming {
        bytes_written: u32,
        bytes_total: u32,
    },
    /// Step 6: Verifying firmware in flash.
    Verifying {
        bytes_verified: u32,
        bytes_total: u32,
    },
    /// Full flash finished successfully.
    Done,
    /// Flash failed at the step that was in progress.
    Failed { at_step: usize, message: String },
}

const MAX_FLASH_ATTEMPTS: u32 = 3;

struct PendingFlash {
    tfw_path: PathBuf,
    attempts: u32,
}

// ── Persistent device registry helper ──────────────────────────────────

/// Look up or mint a persistent `DeviceId` for `uid`. Emits
/// `Event::DeviceCreated` only if this is the first time the UID has been
/// seen — subsequent calls for the same UID return the existing ID
/// without emitting anything.
fn get_or_create_persistent_device(
    uid: ChipUid,
    name: impl Into<String>,
    firmware_id: Option<crate::state::FirmwareId>,
    persistent_devices: &Mutex<HashMap<ChipUid, DeviceId>>,
    next_device_id: &AtomicU64,
    tx: &crossbeam_channel::Sender<Event>,
) -> DeviceId {
    let mut is_new = false;
    let id = {
        let mut registry = persistent_devices.lock().unwrap();
        *registry.entry(uid).or_insert_with(|| {
            is_new = true;
            DeviceId(next_device_id.fetch_add(1, Ordering::Relaxed))
        })
    };
    if is_new {
        let _ = tx.send(Event::DeviceCreated {
            device_id: id,
            name: name.into(),
            kind: DeviceKind::Persistent,
            firmware_id,
        });
    }
    id
}

// ── Bridge state ───────────────────────────────────────────────────────

/// A device owned by the bridge. The device is a trait object —
/// could be an EmulatedDevice, a PhysicalDevice behind a serial
/// connection, etc.
struct BridgeDevice {
    device: Box<dyn Device>,
    cancel: tokio_util::sync::CancellationToken,
}

struct BridgeSerial {
    cancel: tokio_util::sync::CancellationToken,
    _task: tokio::task::JoinHandle<()>,
    /// Live USART2 `SerialSender`, filled by `usart2_connect_loop` once
    /// `Usart2::connect` succeeds and cleared when the connect loop
    /// exits. `None` until then, and always `None` for USART1 entries.
    ///
    /// Keyed by `SerialPortIndex` via the outer `serials` map, not by
    /// `DeviceId` — so `Command::ProbeMoshiMoshi` can reach every live
    /// USART2 wire whether or not Awake has identified its device yet.
    /// That's the whole point of the probe: coax unidentified adapters
    /// into emitting the hello that reveals their identity.
    usart2_sender: Arc<Mutex<Option<Arc<serial::SerialSender>>>>,
}

// ── Main loop ──────────────────────────────────────────────────────────

pub async fn run(
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    cmd_tx: mpsc::UnboundedSender<Command>,
    event_tx: crossbeam_channel::Sender<Event>,
    ctx: egui::Context,
) {
    // Counts USART1 adapters currently in their "settling" phase — from
    // `Command::RegisterSerial` until the connect loop finishes its
    // initial discovery and enters the main read loop (or cancels out).
    // Each USART1 increments on spawn and decrements on settle/exit.
    //
    // When the decrement drops the count to zero, the last USART1 self-
    // enqueues `Command::ProbeMoshiMoshi`. This coalesces the probe into
    // a single fire across the whole "batch" of USART1s that come up
    // together (startup, or a bulk port registration), instead of one
    // probe per USART1. Ports registered individually after the batch
    // settles each trigger their own single probe.
    let usart1_settling: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));

    let devices: Arc<Mutex<HashMap<DeviceId, BridgeDevice>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut serials: HashMap<SerialPortIndex, BridgeSerial> = HashMap::new();

    // Shared persistent device registry: UID → DeviceId.
    // Shared device ID counter — all device IDs come from here to avoid collisions.
    let next_device_id: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let persistent_devices: Arc<Mutex<HashMap<ChipUid, DeviceId>>> =
        Arc::new(Mutex::new(HashMap::new()));
    // Pending flash: device_id → flash state. Checked by USART1 loop after bootrom
    // entry. Entry persists until the full flash succeeds or the retry limit is hit.
    let pending_flash: Arc<Mutex<HashMap<DeviceId, PendingFlash>>> =
        Arc::new(Mutex::new(HashMap::new()));
    // Notified when a new flash request lands — wakes the USART1 loop
    // so it can try to enter debug immediately (without waiting for reset).
    let flash_notify: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());
    // Oneshot receivers waiting for a specific ChipUid to re-enumerate
    // over USB after a stub flash. The USB supervisor fires these when
    // it attaches a fob with the matching serial descriptor.
    let flash_wait_usb: Arc<Mutex<HashMap<ChipUid, (u64, tokio::sync::oneshot::Sender<()>)>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let flash_generation: Arc<std::sync::atomic::AtomicU64> =
        Arc::new(std::sync::atomic::AtomicU64::new(0));

    // Spawn the native-USB supervisor. Owns attach/detach lifecycle for
    // all rcard fobs enumerated over native USB. Uses polling today
    // (nusb 0.1); swap the discovery function for `nusb::watch_devices`
    // when the crate is bumped to 0.2.
    let _usb_supervisor = tokio::spawn(usb_supervisor_loop(
        event_tx.clone(),
        ctx.clone(),
        devices.clone(),
        persistent_devices.clone(),
        next_device_id.clone(),
        flash_wait_usb.clone(),
    ));

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Command::RunEmulator {
                firmware_id,
                tfw_path,
            } => {
                let device_id = DeviceId(next_device_id.fetch_add(1, Ordering::Relaxed));
                let tx = event_tx.clone();
                let repaint = ctx.clone();

                // EmulatedDevice::start() is blocking (spawns Renode), run on blocking pool.
                let cancel = tokio_util::sync::CancellationToken::new();
                let task_cancel = cancel.clone();

                let bridge_devices = devices.clone();
                let _task = tokio::task::spawn_blocking({
                    let tx = tx.clone();
                    let repaint = repaint.clone();
                    move || {
                        match emulator::EmulatedDevice::start(&tfw_path) {
                            Ok(dev) => {
                                use device::adapter::AdapterId;
                                let adapter_ids = vec![
                                    AdapterId(0), // usart1
                                    AdapterId(1), // usart2
                                    AdapterId(2), // renode
                                ];
                                for &(id, name) in &[
                                    (AdapterId(0), "usart1"),
                                    (AdapterId(1), "usart2"),
                                    (AdapterId(2), "renode"),
                                ] {
                                    let _ = tx.send(Event::AdapterCreated {
                                        adapter_id: id,
                                        display_name: name.into(),
                                        // Emulator adapters don't currently
                                        // expose the IPC capability to the
                                        // GUI (see the empty `capabilities`
                                        // on `DeviceCreated` below).
                                        ipc_transport: None,
                                    });
                                }
                                let _ = tx.send(Event::DeviceCreated {
                                    device_id,
                                    name: "Emulator".into(),
                                    kind: DeviceKind::Emulator,
                                    firmware_id: Some(firmware_id),
                                });
                                for &id in &adapter_ids {
                                    let _ = tx.send(Event::AdapterBound {
                                        adapter_id: id,
                                        device_id,
                                        capabilities: vec![],
                                    });
                                }
                                repaint.request_repaint();

                                // Subscribe before moving the device into the
                                // bridge map — the receiver works independently.
                                let mut rx = dev.subscribe();
                                bridge_devices.lock().unwrap().insert(
                                    device_id,
                                    BridgeDevice {
                                        device: Box::new(dev),
                                        cancel: task_cancel.clone(),
                                    },
                                );

                                // Forward events until cancelled or closed.
                                loop {
                                    if task_cancel.is_cancelled() {
                                        break;
                                    }
                                    match rx.try_recv() {
                                        Ok(device::device::DeviceEvent::Log(log)) => {
                                            let _ = tx.send(Event::Log {
                                                device: device_id,
                                                log,
                                            });
                                            repaint.request_repaint();
                                        }
                                        Ok(device::device::DeviceEvent::Error(e)) => {
                                            eprintln!("emulator {device_id:?}: {e:?}");
                                        }
                                        Ok(_) => {}
                                        Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                                            std::thread::sleep(std::time::Duration::from_millis(
                                                10,
                                            ));
                                        }
                                        Err(
                                            tokio::sync::broadcast::error::TryRecvError::Lagged(_),
                                        ) => {}
                                        Err(
                                            tokio::sync::broadcast::error::TryRecvError::Closed,
                                        ) => break,
                                    }
                                }

                                // Remove device from bridge map — the Device
                                // (and its Renode _run_thread) drops here.
                                bridge_devices.lock().unwrap().remove(&device_id);
                                for id in [AdapterId(0), AdapterId(1), AdapterId(2)] {
                                    let _ = tx.send(Event::AdapterRemoved { adapter_id: id });
                                }
                                repaint.request_repaint();
                            }
                            Err(e) => {
                                eprintln!("emulator start failed: {e}");
                                repaint.request_repaint();
                            }
                        }
                    }
                });
            }

            Command::RemoveDevice(id) => {
                if let Some(dev) = devices.lock().unwrap().remove(&id) {
                    dev.cancel.cancel();
                }
            }

            Command::FlashViaSifliDebug {
                device_id,
                firmware_id: _,
                tfw_path,
            } => {
                pending_flash.lock().unwrap().insert(device_id, PendingFlash { tfw_path, attempts: 0 });
                flash_notify.notify_waiters();
                let _ = event_tx.send(Event::FlashProgress {
                    device_id,
                    phase: FlashPhase::Resetting,
                });
                ctx.request_repaint();
            }

            Command::RegisterSerial {
                index,
                port,
                adapter_type,
            } => {
                if let Some(old) = serials.remove(&index) {
                    old.cancel.cancel();
                }

                // Increment the settling counter synchronously *before*
                // spawning the task — multiple `RegisterSerial` commands
                // processed back-to-back all bump the count before any
                // task can run and decrement, so a "batch" (e.g. the
                // startup load of saved ports) stays a single probe fire.
                if matches!(adapter_type, SerialAdapterType::Usart1) {
                    usart1_settling.fetch_add(1, Ordering::SeqCst);
                }

                let cancel = tokio_util::sync::CancellationToken::new();
                let usart2_sender: Arc<Mutex<Option<Arc<serial::SerialSender>>>> =
                    Arc::new(Mutex::new(None));
                let task = tokio::spawn(serial_connect_loop(
                    index,
                    port,
                    adapter_type,
                    event_tx.clone(),
                    ctx.clone(),
                    cancel.clone(),
                    persistent_devices.clone(),
                    next_device_id.clone(),
                    pending_flash.clone(),
                    flash_notify.clone(),
                    flash_wait_usb.clone(),
                    flash_generation.clone(),
                    devices.clone(),
                    usart1_settling.clone(),
                    cmd_tx.clone(),
                    usart2_sender.clone(),
                ));

                serials.insert(
                    index,
                    BridgeSerial {
                        cancel,
                        _task: task,
                        usart2_sender,
                    },
                );
            }

            Command::UnregisterSerial { index } => {
                if let Some(s) = serials.remove(&index) {
                    s.cancel.cancel();
                }
            }

            Command::Build {
                build_id,
                firmware_dir,
                config,
                board,
                layout,
                out,
            } => {
                let build_tx = event_tx.clone();
                let build_ctx = ctx.clone();
                tokio::task::spawn_blocking(move || {
                    let tx = build_tx.clone();
                    let repaint = build_ctx.clone();
                    let on_event = move |event: tfw::build::BuildEvent| {
                        use tfw::build::*;
                        // Forward strongly-typed events first; opportunistically
                        // echo interesting lines into the free-form pipeline log
                        // so the collapsed raw view still has context.
                        let echo: Option<String> = match &event {
                            BuildEvent::Build(state) => {
                                let phase = map_build_state(state);
                                let _ = tx.send(Event::BuildPhase {
                                    build_id,
                                    phase: phase.clone(),
                                });
                                Some(format!("stage: {}", phase.label()))
                            }
                            BuildEvent::ConfigResolved(resolved) => {
                                let memories = resolved
                                    .memories
                                    .iter()
                                    .map(|m| MemoryDevice {
                                        name: m.name.clone(),
                                        size: m.size,
                                        mappings: m.mappings.clone(),
                                    })
                                    .collect();
                                let place_capacities = resolved.places.clone();
                                let tasks = resolved.tasks.clone();
                                let _ = tx.send(Event::BuildConfigResolved {
                                    build_id,
                                    uuid: resolved.build_id.clone(),
                                    name: resolved.name.clone(),
                                    memories,
                                    place_capacities,
                                    tasks,
                                });
                                Some(format!("build id: {}", resolved.build_id))
                            }
                            BuildEvent::IpcMetadata(bundle) => {
                                // Derive the same `Vec<ResourceSummary>` a
                                // loaded-firmware snapshot would produce,
                                // so the Resources card fills in for live
                                // builds without any UI branching.
                                let resources = ResourceSummary::list_from_bundle(bundle);
                                let n_res = bundle.resources.len();
                                let n_srv = bundle.servers.len();
                                let _ = tx.send(Event::BuildResources {
                                    build_id,
                                    resources,
                                    bundle: bundle.clone(),
                                });
                                Some(format!(
                                    "ipc metadata: {} resources, {} servers",
                                    n_res, n_srv
                                ))
                            }
                            BuildEvent::Crate {
                                name,
                                kind,
                                update: ResourceUpdate::State(state),
                            } => {
                                let gui_kind = classify_kind(name, *kind);
                                let gui_state = map_crate_state(state);
                                let _ = tx.send(Event::BuildCrateState {
                                    build_id,
                                    name: name.clone(),
                                    kind: gui_kind,
                                    state: gui_state,
                                });
                                Some(format!("  {:?} {name}", state))
                            }
                            BuildEvent::Crate {
                                name,
                                kind,
                                update: ResourceUpdate::Event(event),
                            } => {
                                let gui_kind = classify_kind(name, *kind);
                                match event {
                                    CrateEvent::Sized { region, size } => {
                                        let _ = tx.send(Event::BuildCrateSized {
                                            build_id,
                                            name: name.clone(),
                                            kind: gui_kind,
                                            region: region.clone(),
                                            size: *size,
                                        });
                                        Some(format!("  Measured {name}.{region} = {size} bytes"))
                                    }
                                    CrateEvent::CargoMessage(m) => {
                                        forward_cargo_message(&tx, build_id, name, gui_kind, m)
                                    }
                                    CrateEvent::CargoError(e) => {
                                        let rendered = format!("{e}");
                                        let _ = tx.send(Event::BuildCrateCargoLine {
                                            build_id,
                                            name: name.clone(),
                                            kind: gui_kind,
                                            line: rendered.clone(),
                                        });
                                        let _ = tx.send(Event::BuildCrateState {
                                            build_id,
                                            name: name.clone(),
                                            kind: gui_kind,
                                            state: CrateBuildState::Failed,
                                        });
                                        Some(rendered)
                                    }
                                }
                            }
                            BuildEvent::HostCrate {
                                name,
                                update: ResourceUpdate::State(state),
                            } => {
                                let gui_state = map_host_crate_state(state);
                                let _ = tx.send(Event::BuildHostCrateState {
                                    build_id,
                                    name: name.clone(),
                                    state: gui_state,
                                });
                                Some(format!("  {name}: {:?}", state))
                            }
                            BuildEvent::HostCrate {
                                name,
                                update: ResourceUpdate::Event(event),
                            } => match event {
                                HostCrateEvent::CargoMessage(m) => forward_cargo_message(
                                    &tx,
                                    build_id,
                                    name,
                                    crate::state::CrateKind::HostCrate,
                                    m,
                                ),
                                HostCrateEvent::CargoError(e) => {
                                    let rendered = format!("{e}");
                                    let _ = tx.send(Event::BuildCrateCargoLine {
                                        build_id,
                                        name: name.clone(),
                                        kind: crate::state::CrateKind::HostCrate,
                                        line: rendered.clone(),
                                    });
                                    let _ = tx.send(Event::BuildHostCrateState {
                                        build_id,
                                        name: name.clone(),
                                        state: HostCrateBuildState::Failed,
                                    });
                                    Some(rendered)
                                }
                            },
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
                                let _ = tx.send(Event::BuildAllocation {
                                    build_id,
                                    allocation: MemoryAllocation {
                                        place: place.clone(),
                                        owner: owner.clone(),
                                        region: region.clone(),
                                        base: *base,
                                        size: *size,
                                        requested_place: request.requested_place.clone(),
                                    },
                                });
                                Some(format!(
                                    "  alloc {owner}.{region} in {place} @ {base:#010x} ({size}B)"
                                ))
                            }
                            BuildEvent::Memory { .. } => None,
                            BuildEvent::Image(ResourceUpdate::State(state)) => match state {
                                ImageState::Assembled { size } => {
                                    let _ = tx.send(Event::BuildImage {
                                        build_id,
                                        image: ImageProgress::Assembled { size: *size },
                                    });
                                    Some(format!("  Image assembled: {size} bytes"))
                                }
                                ImageState::Archived { path } => {
                                    let size =
                                        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                                    let _ = tx.send(Event::BuildImage {
                                        build_id,
                                        image: ImageProgress::Archived {
                                            size,
                                            path: path.clone(),
                                        },
                                    });
                                    Some(format!("  Archived: {}", path.display()))
                                }
                            },
                            BuildEvent::Image(ResourceUpdate::Event(event)) => match event {
                                ImageEvent::PlaceWritten { place, dest, .. } => {
                                    Some(format!("  Place {place} @ {dest:#010x}"))
                                }
                            },
                        };
                        if let Some(message) = echo {
                            let _ = tx.send(Event::BuildLog { build_id, message });
                        }
                        repaint.request_repaint();
                    };

                    let result = tfw::build::build(
                        &firmware_dir,
                        &config,
                        &board,
                        &layout,
                        &out,
                        Some(&on_event),
                        None,
                    );

                    let _ = build_tx.send(Event::BuildComplete {
                        build_id,
                        result: result.map_err(|e| e.to_string()),
                    });
                    build_ctx.request_repaint();
                });
            }

            Command::IpcCall {
                device_id,
                call,
                reply,
            } => {
                // Single capability lookup — `crate::ipc::pick` walks all
                // registered `Ipc`s on the device and returns the
                // highest-priority one (USB beats USART2 today). This
                // handler doesn't have to know which transports exist.
                let devices_lock = devices.lock().unwrap();
                let Some(bridge_dev) = devices_lock.get(&device_id) else {
                    let _ = reply.send(Err("device not found in bridge".into()));
                    continue;
                };

                let Some(ipc) = crate::ipc::pick(&*bridge_dev.device) else {
                    let _ = reply.send(Err(
                        "device has no IPC capability (no USART2 or USB transport)".into(),
                    ));
                    continue;
                };

                tokio::spawn(async move {
                    eprintln!(
                        "[ipc] call via {} task={} op={:02x}.{:02x} args={:02x?}",
                        ipc.label(),
                        call.task_id,
                        call.resource_kind,
                        call.method_id,
                        call.wire_args,
                    );
                    let lease_refs: Vec<&[u8]> =
                        call.lease_data.iter().map(|d| d.as_slice()).collect();
                    let req = rcard_usb_proto::IpcRequest {
                        task_id: call.task_id,
                        resource_kind: call.resource_kind,
                        method: call.method_id,
                        args: &call.wire_args,
                        leases: &call.leases,
                        lease_data: &lease_refs,
                    };
                    let result = ipc.call(&req).await.map_err(|e| e.to_string());
                    match &result {
                        Ok(r) => eprintln!(
                            "[ipc] reply via {} rc={} return={:02x?}",
                            ipc.label(),
                            r.rc,
                            r.return_value,
                        ),
                        Err(e) => eprintln!("[ipc] error via {}: {}", ipc.label(), e),
                    }
                    let _ = reply.send(result);
                });
            }

            Command::SendMoshiMoshi { device_id } => {
                // Manual diagnostic: fire MoshiMoshi on the device's USART2
                // adapter on demand. Looks up the raw SerialSender capability
                // (registered alongside the unified Ipc by `Usart2::capabilities`)
                // so we can send a TYPE_CONTROL_REQUEST frame directly,
                // bypassing the IPC-request path.
                let devices_lock = devices.lock().unwrap();
                let Some(bridge_dev) = devices_lock.get(&device_id) else {
                    eprintln!("[probe] SendMoshiMoshi: device {device_id:?} not in bridge");
                    continue;
                };

                use device::device::DeviceExt;
                let Some(sender) = bridge_dev.device.get::<serial::SerialSender>() else {
                    eprintln!(
                        "[probe] SendMoshiMoshi: device {device_id:?} has no USART2 SerialSender capability"
                    );
                    continue;
                };

                eprintln!("[probe] SendMoshiMoshi: firing on device {device_id:?}");
                tokio::spawn(async move {
                    match sender.send_moshi_moshi().await {
                        Ok(()) => eprintln!("[probe] SendMoshiMoshi: sent"),
                        Err(e) => eprintln!("[probe] SendMoshiMoshi: send failed: {e}"),
                    }
                });
            }

            Command::FlashComplete { device_id } => {
                pending_flash.lock().unwrap().remove(&device_id);
                let _ = event_tx.send(Event::FlashProgress {
                    device_id,
                    phase: FlashPhase::Done,
                });
                ctx.request_repaint();
            }

            Command::FlashFailed { device_id, message } => {
                pending_flash.lock().unwrap().remove(&device_id);
                let _ = event_tx.send(Event::FlashProgress {
                    device_id,
                    phase: FlashPhase::Failed { at_step: 4, message },
                });
                ctx.request_repaint();
            }

            Command::FlashPhaseUpdate { device_id, phase } => {
                let _ = event_tx.send(Event::FlashProgress {
                    device_id,
                    phase,
                });
                ctx.request_repaint();
            }

            Command::ProbeMoshiMoshi => {
                // Fire MoshiMoshi on every registered USART2 serial
                // adapter, regardless of whether its device has been
                // identified yet — identifying unknown adapters is the
                // whole point of the probe. Iterates `serials` (keyed
                // by `SerialPortIndex`) rather than `devices` (keyed by
                // `DeviceId`) so a USART2 wire that hasn't yet seen its
                // Awake still gets probed.
                let targets: Vec<(SerialPortIndex, Arc<serial::SerialSender>)> = serials
                    .iter()
                    .filter_map(|(idx, bs)| {
                        bs.usart2_sender.lock().unwrap().clone().map(|s| (*idx, s))
                    })
                    .collect();

                if targets.is_empty() {
                    eprintln!("[probe] ProbeMoshiMoshi: no live USART2 adapters, skipping");
                    continue;
                }

                eprintln!(
                    "[probe] ProbeMoshiMoshi: firing on {} USART2 adapter(s)",
                    targets.len()
                );
                for (idx, sender) in targets {
                    tokio::spawn(async move {
                        match sender.send_moshi_moshi().await {
                            Ok(()) => {
                                eprintln!("[probe] ProbeMoshiMoshi: sent to serial[{idx}]")
                            }
                            Err(e) => eprintln!(
                                "[probe] ProbeMoshiMoshi: send failed on serial[{idx}]: {e}"
                            ),
                        }
                    });
                }
            }
        }
    }
}

// ── Serial connection loop ─────────────────────────────────────────────

async fn serial_connect_loop(
    index: SerialPortIndex,
    port: String,
    adapter_type: SerialAdapterType,
    tx: crossbeam_channel::Sender<Event>,
    ctx: egui::Context,
    cancel: tokio_util::sync::CancellationToken,
    persistent_devices: Arc<Mutex<HashMap<ChipUid, DeviceId>>>,
    next_device_id: Arc<AtomicU64>,
    pending_flash: Arc<Mutex<HashMap<DeviceId, PendingFlash>>>,
    flash_notify: Arc<tokio::sync::Notify>,
    flash_wait_usb: Arc<Mutex<HashMap<ChipUid, (u64, tokio::sync::oneshot::Sender<()>)>>>,
    flash_generation: Arc<std::sync::atomic::AtomicU64>,
    bridge_devices: Arc<Mutex<HashMap<DeviceId, BridgeDevice>>>,
    // USART1-specific settling counter + cmd_tx for self-enqueueing
    // the batched ProbeMoshiMoshi. USART2 doesn't touch these.
    usart1_settling: Arc<AtomicUsize>,
    cmd_tx: mpsc::UnboundedSender<Command>,
    // USART2-specific sender slot — filled by the USART2 connect loop
    // once `Usart2::connect` succeeds so `ProbeMoshiMoshi` can find the
    // wire even before an Awake identifies the device. Unused by USART1.
    usart2_sender: Arc<Mutex<Option<Arc<serial::SerialSender>>>>,
) {
    match adapter_type {
        SerialAdapterType::Usart1 => {
            usart1_connect_loop(
                index,
                port,
                tx,
                ctx,
                cancel,
                persistent_devices,
                next_device_id,
                pending_flash,
                flash_notify,
                flash_wait_usb,
                flash_generation,
                usart1_settling,
                cmd_tx,
            )
            .await;
        }
        SerialAdapterType::Usart2 => {
            usart2_connect_loop(
                index,
                port,
                tx,
                ctx,
                cancel,
                persistent_devices,
                next_device_id,
                bridge_devices,
                usart2_sender,
            )
            .await;
        }
    }
}

/// Decrements `usart1_settling` on drop. If the decrement takes the count
/// to zero, self-enqueues `Command::ProbeMoshiMoshi` so the last USART1
/// to settle (or cancel) triggers one batched probe across all devices.
///
/// Using Drop (rather than an explicit `.release()` call) means
/// cancellation paths — `return`s from port-open failures, the
/// `cancel.cancelled()` branch in the select loop, panic unwinds — all
/// balance the earlier `fetch_add(1)` in `Command::RegisterSerial`
/// without per-site cleanup code.
struct Usart1SettleGuard {
    counter: Arc<AtomicUsize>,
    cmd_tx: mpsc::UnboundedSender<Command>,
}

impl Drop for Usart1SettleGuard {
    fn drop(&mut self) {
        let prev = self.counter.fetch_sub(1, Ordering::SeqCst);
        if prev == 1 {
            let _ = self.cmd_tx.send(Command::ProbeMoshiMoshi);
        }
    }
}

/// USART1 connection loop — reads text lines, detects sentinels,
/// manages device lifecycle through boot phases.
async fn usart1_connect_loop(
    index: SerialPortIndex,
    port: String,
    tx: crossbeam_channel::Sender<Event>,
    ctx: egui::Context,
    cancel: tokio_util::sync::CancellationToken,
    persistent_devices: Arc<Mutex<HashMap<ChipUid, DeviceId>>>,
    next_device_id: Arc<AtomicU64>,
    pending_flash: Arc<Mutex<HashMap<DeviceId, PendingFlash>>>,
    flash_notify: Arc<tokio::sync::Notify>,
    flash_wait_usb: Arc<Mutex<HashMap<ChipUid, (u64, tokio::sync::oneshot::Sender<()>)>>>,
    flash_generation: Arc<std::sync::atomic::AtomicU64>,
    usart1_settling: Arc<AtomicUsize>,
    cmd_tx: mpsc::UnboundedSender<Command>,
) {
    // Guard is held until the connect loop either settles (in which case
    // we drop it manually below to fire the probe as we enter the read
    // loop) or cancels/errors out (in which case the function's drop
    // fires it, possibly triggering a probe for any already-ready peers).
    let mut settle_guard = Some(Usart1SettleGuard {
        counter: usart1_settling,
        cmd_tx,
    });
    let adapter_id = device::adapter::AdapterId(index as u64);

    loop {
        let _ = tx.send(Event::SerialStatus {
            index,
            status: SerialPortStatus::Connecting,
        });
        ctx.request_repaint();

        let conn = match serial::Usart1Connection::open(&port) {
            Ok(c) => c,
            Err(_) => {
                let _ = tx.send(Event::SerialStatus {
                    index,
                    status: SerialPortStatus::Error,
                });
                ctx.request_repaint();
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
                }
                continue;
            }
        };

        // Destructure into the line reader (owned by the parallel reader
        // task) and the shared SifliDebug handle (used for discovery and
        // flash operations from this main loop).
        let serial::Usart1Connection {
            mut reader,
            sifli_debug,
        } = conn;

        let _ = tx.send(Event::AdapterCreated {
            adapter_id,
            display_name: format!("USART1 ({})", port),
            // USART1 carries SifliDebug, not IPC.
            ipc_transport: None,
        });
        let _ = tx.send(Event::SerialStatus {
            index,
            status: SerialPortStatus::PortOpen,
        });
        ctx.request_repaint();

        // Parallel line reader. Runs concurrently with `try_discover_usart1`
        // so the boot-time text backlog (SFBL, kernel Awake, etc.) gets
        // timestamped at real byte-arrival time — otherwise we'd block on
        // discovery for ~1s and every buffered line would land in the
        // channel *after* any USART2 logs that raced ahead.
        let (line_tx, mut line_rx) =
            tokio::sync::mpsc::unbounded_channel::<(std::time::Instant, String)>();
        let reader_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut line = String::new();
            let mut line_start: Option<std::time::Instant> = None;
            let mut buf = [0u8; 256];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => return,
                    Ok(n) => {
                        for &byte in &buf[..n] {
                            if byte == b'\n' {
                                let text = std::mem::take(&mut line);
                                let start =
                                    line_start.take().unwrap_or_else(std::time::Instant::now);
                                if line_tx.send((start, text)).is_err() {
                                    return;
                                }
                            } else {
                                if line_start.is_none() {
                                    line_start = Some(std::time::Instant::now());
                                }
                                line.push(byte as char);
                            }
                        }
                    }
                    Err(_) => return,
                }
            }
        });

        // Discovery handshake: try to identify a device already attached to
        // this port. Mirrors the shape of the SFBL bootrom-entry flow but
        // skips the stability wait (we weren't the ones who caused a reset).
        let outcome = try_discover_usart1(&sifli_debug, &port).await;
        let mut current_device: Option<DeviceId> = register_from_discovery(
            outcome,
            &port,
            adapter_id,
            &tx,
            &persistent_devices,
            &next_device_id,
            &ctx,
        );
        if current_device.is_some() {
            let _ = tx.send(Event::SerialStatus {
                index,
                status: SerialPortStatus::DeviceDetected,
            });
        }

        // Settle: we're past discovery and the select! loop below is
        // about to start consuming `line_rx`. Dropping the guard here
        // decrements `usart1_settling`; if this was the last USART1 in
        // the batch, the drop fires a single `Command::ProbeMoshiMoshi`
        // that pings every USART2 device so their supervisors emit the
        // `hello` line we're now ready to receive.
        settle_guard.take();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    reader_task.abort();
                    let _ = tx.send(Event::AdapterRemoved { adapter_id });
                    ctx.request_repaint();
                    return;
                }
                _ = flash_notify.notified() => {
                    // Flash was requested. Try to reset the device into
                    // bootrom via SifliDebug so the existing SFBL path can
                    // take over. If we can't enter debug, fall back to
                    // asking the user to reset the device manually.
                    eprintln!("[usart1:{port}] flash requested — attempting auto-reset");
                    match sifli_debug.try_acquire().await {
                        Some(session) => {
                            // AIRCR: VECTKEY (0x05FA) | SYSRESETREQ (bit 2)
                            let _ = session
                                .mem_write_no_response(0xE000_ED0C, &[0x05FA_0004])
                                .await;
                            // Chip is rebooting — skip the Drop-spawned Exit
                            // since it would just time out against a dead link.
                            session.forget();
                            eprintln!("[usart1:{port}] auto-reset issued, resyncing tap on SFBL");
                            // The AIRCR write's ACK may have been cut off
                            // mid-frame by the reset, leaving the tap parser
                            // stuck. Put the tap into sentinel-resync mode so
                            // it forwards every byte (including any garbled
                            // tail of the truncated ACK) as passthrough noise
                            // until the bootrom's "SFBL\n" marker arrives.
                            //
                            // 1s timeout: if SFBL doesn't show up by then,
                            // the auto-reset probably didn't take. Fall back
                            // to asking the user to reset manually. The tap
                            // stays in resync mode in the background, so when
                            // the user does press reset and SFBL arrives, the
                            // existing line-based handler picks it up.
                            match sifli_debug
                                .resync_on_sentinel(
                                    b"SFBL\n".to_vec(),
                                    std::time::Duration::from_secs(1),
                                )
                                .await
                            {
                                Ok(()) => {
                                    eprintln!("[usart1:{port}] tap resync complete");
                                }
                                Err(serial::sifli_debug::Error::Timeout) => {
                                    eprintln!("[usart1:{port}] auto-reset didn't trigger SFBL within 1s — asking user to reset manually");
                                    if let Some(dev_id) = current_device {
                                        let _ = tx.send(Event::FlashProgress {
                                            device_id: dev_id,
                                            phase: FlashPhase::WaitingForReset,
                                        });
                                        ctx.request_repaint();
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[usart1:{port}] resync_on_sentinel failed: {e}");
                                    break;
                                }
                            }
                        }
                        None => {
                            eprintln!("[usart1:{port}] auto-reset failed — asking user to reset manually");
                            if let Some(dev_id) = current_device {
                                let _ = tx.send(Event::FlashProgress {
                                    device_id: dev_id,
                                    phase: FlashPhase::WaitingForReset,
                                });
                                ctx.request_repaint();
                            }
                        }
                    }
                    continue;
                }
                item = line_rx.recv() => {
                    let Some((line_received_at, raw_line)) = item else {
                        // Reader task ended → port closed / error.
                        break;
                    };

                    let trimmed = raw_line.trim_end_matches('\r');
                    let (device_tick, line) = parse_tick_prefix(trimmed);

                    // Forward the stripped line to the serial adapter terminal.
                    let _ = tx.send(Event::SerialRawLine {
                        index,
                        line: line.clone(),
                    });

                    // ── Sentinel: SFBL — bootrom entered ───────────
                    // `ends_with` rather than equality: after a tap resync,
                    // the first line may have a garbled prefix from the
                    // truncated ACK tail, e.g. "\x06SFBL".
                    if line.ends_with("SFBL") {
                        // Device just (re)booted. Unbind adapter from
                        // current device — persistent devices survive;
                        // ephemeral ones auto-clean via 0-adapter rule.
                        if let Some(old_id) = current_device.take() {
                            let _ = tx.send(Event::AdapterUnbound {
                                adapter_id,
                                device_id: old_id,
                            });
                        }

                        // Create ephemeral device immediately so the UI shows it.
                        let ephemeral_id = DeviceId(next_device_id.fetch_add(1, Ordering::Relaxed));

                        let _ = tx.send(Event::DeviceCreated {
                            device_id: ephemeral_id,
                            name: format!("{} (USART1)", port),
                            kind: DeviceKind::Ephemeral,
                            firmware_id: None,
                        });
                        let _ = tx.send(Event::AdapterBound {
                            adapter_id,
                            device_id: ephemeral_id,
                            capabilities: vec![KnownCapability::SifliDebug],
                        });
                        let _ = tx.send(Event::DevicePhaseChanged {
                            device_id: ephemeral_id,
                            phase: DevicePhase::Stabilizing,
                        });
                        let _ = tx.send(Event::SerialStatus {
                            index,
                            status: SerialPortStatus::DeviceDetected,
                        });
                        current_device = Some(ephemeral_id);
                        ctx.request_repaint();

                        // Wait for power stability — if we see another SFBL
                        // within 1s, the device is brown-out resetting. Reset
                        // the timer each time and only proceed once stable.
                        let stability_delay = std::time::Duration::from_secs(1);
                        loop {
                            tokio::select! {
                                _ = cancel.cancelled() => {
                                    reader_task.abort();
                                    let _ = tx.send(Event::AdapterRemoved { adapter_id });
                                    ctx.request_repaint();
                                    return;
                                }
                                _ = tokio::time::sleep(stability_delay) => {
                                    // 1s with no SFBL — power is stable.
                                    break;
                                }
                                item = line_rx.recv() => {
                                    let Some((_, sfbl_buf)) = item else {
                                        break; // port closed
                                    };
                                    let l = sfbl_buf.trim_end_matches('\r');
                                    if l.ends_with("SFBL") {
                                        eprintln!("[usart1:{port}] SFBL again (power unstable), resetting timer");
                                        // Timer resets by looping back to select!
                                    } else {
                                        // Non-SFBL line during stability wait — device
                                        // booted past bootrom already. Break and proceed.
                                        break;
                                    }
                                }
                            }
                        }

                        // Power stable — now in bootrom.
                        let _ = tx.send(Event::DevicePhaseChanged {
                            device_id: ephemeral_id,
                            phase: DevicePhase::Bootrom,
                        });
                        ctx.request_repaint();

                        // Enter SifliDebug and read the chip UID from eFuse bank 0.
                        let t0 = std::time::Instant::now();
                        if let Some(session) = sifli_debug.try_acquire().await {
                            eprintln!("[usart1:{port}] entered debug in {:?}", t0.elapsed());
                            match efuse_read_uid(&session).await {
                                Ok(uid) => {
                                    eprintln!("[usart1:{port}] identified device: {uid}");

                                    // GetOrCreate persistent device for this UID.
                                    let persistent_id = get_or_create_persistent_device(
                                        uid,
                                        format!("Device {uid}"),
                                        None,
                                        &persistent_devices,
                                        &next_device_id,
                                        &tx,
                                    );
                                    let _ = tx.send(Event::AdapterBound {
                                        adapter_id,
                                        device_id: persistent_id,
                                        capabilities: vec![KnownCapability::SifliDebug],
                                    });
                                    let _ = tx.send(Event::DeviceUpgraded {
                                        old_id: ephemeral_id,
                                        new_id: persistent_id,
                                    });
                                    let _ = tx.send(Event::DevicePhaseChanged {
                                        device_id: persistent_id,
                                        phase: DevicePhase::Bootrom,
                                    });
                                    current_device = Some(persistent_id);
                                    ctx.request_repaint();

                                    // Check for pending flash on this device.
                                    let flash_tfw = {
                                        let mut map = pending_flash.lock().unwrap();
                                        if let Some(pf) = map.get_mut(&persistent_id) {
                                            pf.attempts += 1;
                                            if pf.attempts > MAX_FLASH_ATTEMPTS {
                                                let pf = map.remove(&persistent_id).unwrap();
                                                eprintln!(
                                                    "[usart1:{port}] flash retry limit ({MAX_FLASH_ATTEMPTS}) exceeded"
                                                );
                                                let _ = tx.send(Event::FlashProgress {
                                                    device_id: persistent_id,
                                                    phase: FlashPhase::Failed {
                                                        at_step: 0,
                                                        message: format!(
                                                            "device reset {} times, giving up",
                                                            pf.attempts - 1,
                                                        ),
                                                    },
                                                });
                                                ctx.request_repaint();
                                                None
                                            } else {
                                                Some(pf.tfw_path.clone())
                                            }
                                        } else {
                                            None
                                        }
                                    };
                                    if let Some(_tfw_path) = flash_tfw {
                                        let attempt = pending_flash.lock().unwrap()
                                            .get(&persistent_id).map(|pf| pf.attempts).unwrap_or(1);
                                        eprintln!("[usart1:{port}] pending flash — writing stub (attempt {attempt}/{MAX_FLASH_ATTEMPTS})");
                                        let _ = tx.send(Event::FlashProgress {
                                            device_id: persistent_id,
                                            phase: FlashPhase::Resetting,
                                        });
                                        ctx.request_repaint();

                                        match flash_stub_via_debug(&session, &tx, persistent_id, &ctx).await {
                                            Ok(()) => {
                                                eprintln!("[usart1:{port}] stub written, waiting for USB");
                                                // Register a oneshot with the USB supervisor;
                                                // it fires when a fob with this chip UID enumerates
                                                // over native USB. No descriptor polling here —
                                                // the supervisor is the single source of truth for
                                                // USB presence, and its attach event means the
                                                // adapter is fully claimed and IPC is live, not
                                                // just that the OS saw the descriptor.
                                                let generation = flash_generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                                let (wait_tx, wait_rx) = tokio::sync::oneshot::channel::<()>();
                                                flash_wait_usb.lock().unwrap().insert(uid, (generation, wait_tx));
                                                let flash_tx = tx.clone();
                                                let flash_ctx = ctx.clone();
                                                let flash_port = port.clone();
                                                let flash_wait_usb_cleanup = flash_wait_usb.clone();
                                                tokio::spawn(async move {
                                                    let result = tokio::time::timeout(
                                                        std::time::Duration::from_secs(30),
                                                        wait_rx,
                                                    )
                                                    .await;
                                                    match result {
                                                        Ok(Ok(())) => {
                                                            eprintln!("[usart1:{flash_port}] stub USB up");
                                                            let _ = flash_tx.send(Event::FlashProgress {
                                                                device_id: persistent_id,
                                                                phase: FlashPhase::StubBooted,
                                                            });
                                                        }
                                                        _ => {
                                                            let mut map = flash_wait_usb_cleanup.lock().unwrap();
                                                            let superseded = !matches!(map.get(&uid), Some((g, _)) if *g == generation);
                                                            if !superseded {
                                                                map.remove(&uid);
                                                            }
                                                            drop(map);
                                                            if superseded {
                                                                eprintln!("[usart1:{flash_port}] superseded by newer flash, ignoring");
                                                            } else {
                                                                eprintln!("[usart1:{flash_port}] stub USB never appeared");
                                                                let _ = flash_tx.send(Event::FlashProgress {
                                                                    device_id: persistent_id,
                                                                    phase: FlashPhase::Failed {
                                                                        at_step: 3,
                                                                        message: "stub USB device did not appear within 30s".into(),
                                                                    },
                                                                });
                                                            }
                                                        }
                                                    }
                                                    flash_ctx.request_repaint();
                                                });
                                            }
                                            Err(e) => {
                                                eprintln!("[usart1:{port}] stub flash failed: {e}");
                                                let _ = tx.send(Event::FlashProgress {
                                                    device_id: persistent_id,
                                                    phase: FlashPhase::Failed {
                                                        at_step: 1,
                                                        message: e,
                                                    },
                                                });
                                                ctx.request_repaint();
                                            }
                                        }
                                    }
                                    // If no pending flash, session drops → exits debug → device boots.
                                }
                                Err(e) => {
                                    eprintln!("[usart1:{port}] UID read failed: {e}");
                                }
                            }
                            // Session drops here → exits debug mode if still held.
                        } else {
                            eprintln!("[usart1:{port}] failed to enter SifliDebug (timeout)");
                        }
                    }

                    // ── Sentinel: bootloader awake ─────────────────
                    if line == "bootloader: Awake" {
                        if let Some(dev_id) = current_device {
                            let _ = tx.send(Event::DevicePhaseChanged {
                                device_id: dev_id,
                                phase: DevicePhase::Bootloader,
                            });
                            ctx.request_repaint();
                        }
                    }

                    // ── Sentinel: kernel awake ─────────────────────
                    if line == "kernel: Awake" {
                        if let Some(dev_id) = current_device {
                            let _ = tx.send(Event::DevicePhaseChanged {
                                device_id: dev_id,
                                phase: DevicePhase::Kernel,
                            });
                            ctx.request_repaint();
                        }
                    }

                    // ── Sentinel: hello (supervisor-side response to the
                    // USART2 MoshiMoshi probe). Carries chip UID + build id
                    // read direct from efuse — this is the authoritative
                    // identity for the wire. If we're bound to a different
                    // DeviceId (stale ephemeral from SFBL, cable moved to
                    // another device mid-session, etc.), detach the old
                    // adapter attachment and rebind. Same-DeviceId is a
                    // silent no-op: repeated MoshiMoshi pings change nothing.
                    if let Some((uid_bytes, build_id_bytes)) = parse_hello_line(&line) {
                        let chip_uid = ChipUid(uid_bytes);

                        // Get-or-mint the persistent DeviceId for this UID.
                        // Shared registry with USART2/USB, so the same chip
                        // always resolves to the same id regardless of which
                        // transport identified it first.
                        let persistent_id = get_or_create_persistent_device(
                            chip_uid,
                            format!("Device {chip_uid}"),
                            None,
                            &persistent_devices,
                            &next_device_id,
                            &tx,
                        );

                        if current_device != Some(persistent_id) {
                            // Unbind from prior device — it was wrong (or
                            // ephemeral). The adapter persists; only the
                            // binding changes.
                            if let Some(old_id) = current_device.take() {
                                eprintln!(
                                    "[usart1:{port}] discovery: hello uid {chip_uid} \
                                    supersedes bound {old_id:?}, rebinding"
                                );
                                let _ = tx.send(Event::AdapterUnbound {
                                    adapter_id,
                                    device_id: old_id,
                                });
                            }

                            eprintln!(
                                "[usart1:{port}] discovery: identified device \
                                {chip_uid} ({persistent_id:?}) via hello"
                            );

                            // Bind this adapter to the persistent device.
                            let _ = tx.send(Event::AdapterBound {
                                adapter_id,
                                device_id: persistent_id,
                                capabilities: vec![KnownCapability::SifliDebug],
                            });
                            // Build id arrives via the dedicated event so
                            // state.rs can resolve it against loaded .tfw
                            // archives — same pattern as USART2 awake.
                            let _ = tx.send(Event::DeviceReportedBuildId {
                                device_id: persistent_id,
                                build_id_bytes,
                            });
                            // Hello only fires from a running Hubris kernel
                            // (sysmodule_log triggers it), so the phase is
                            // unambiguously Kernel.
                            let _ = tx.send(Event::DevicePhaseChanged {
                                device_id: persistent_id,
                                phase: DevicePhase::Kernel,
                            });
                            let _ = tx.send(Event::SerialStatus {
                                index,
                                status: SerialPortStatus::DeviceDetected,
                            });
                            current_device = Some(persistent_id);
                            ctx.request_repaint();
                        }
                    }

                    if let Some(dev_id) = current_device {
                        let _ = tx.send(Event::Log {
                            device: dev_id,
                            log: device::logs::Log {
                                adapter: adapter_id,
                                contents: device::logs::LogContents::Text(line),
                                received_at: line_received_at,
                                device_tick,
                            },
                        });
                        ctx.request_repaint();
                    }
                }
            }
        }

        // Port lost — remove adapter. Ephemeral devices auto-clean via
        // the 0-adapter rule. Persistent devices just go disconnected.
        current_device.take();
        reader_task.abort();
        let _ = tx.send(Event::AdapterRemoved { adapter_id });
        ctx.request_repaint();

        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
        }
    }
}

/// USART2 connection loop — opens the port and binds a device when the
/// firmware's `Awake` control message arrives (carrying the chip UID).
///
/// Analogue of USART1's SFBL path, but simpler: the Awake payload *is*
/// the identification, so there's no ephemeral → persistent dance. Every
/// Awake goes straight to the persistent-device registry.
async fn usart2_connect_loop(
    index: SerialPortIndex,
    port: String,
    tx: crossbeam_channel::Sender<Event>,
    ctx: egui::Context,
    cancel: tokio_util::sync::CancellationToken,
    persistent_devices: Arc<Mutex<HashMap<ChipUid, DeviceId>>>,
    next_device_id: Arc<AtomicU64>,
    bridge_devices: Arc<Mutex<HashMap<DeviceId, BridgeDevice>>>,
    // Shared slot the `ProbeMoshiMoshi` handler reads to find live USART2
    // wires. Populated here after a successful `Usart2::connect`; cleared
    // when this loop exits so probes don't fire on a dead sender.
    usart2_sender_slot: Arc<Mutex<Option<Arc<serial::SerialSender>>>>,
) {
    let adapter_id = device::adapter::AdapterId(index as u64 + 500_000);

    loop {
        let _ = tx.send(Event::SerialStatus {
            index,
            status: SerialPortStatus::Connecting,
        });
        ctx.request_repaint();

        // Standalone broadcast channel for event forwarding. No
        // PhysicalDevice yet — just plumbing so logs decoded off
        // USART2 can reach the serial adapter panel before a device
        // has been identified.
        let (events_tx, _) = tokio::sync::broadcast::channel(256);
        let sink = device::device::LogSink::new(adapter_id, events_tx.clone());
        let mut rx = events_tx.subscribe();

        let connect_result = serial::Usart2::connect(&port, adapter_id, sink);

        match connect_result {
            Ok(adapter) => {
                // Publish the sender so `ProbeMoshiMoshi` can find this
                // wire even before Awake has identified its device.
                // Cleared on every exit path from this loop-iteration
                // branch (see the `*slot = None` sites further down).
                *usart2_sender_slot.lock().unwrap() = Some(adapter.sender());

                // Hold the adapter until Awake identifies a device.
                let mut adapter = Some(adapter);

                let _ = tx.send(Event::AdapterCreated {
                    adapter_id,
                    display_name: format!("USART2 ({})", port),
                    ipc_transport: Some(("usart2", serial::USART2_IPC_PRIORITY)),
                });
                let _ = tx.send(Event::SerialStatus {
                    index,
                    status: SerialPortStatus::PortOpen,
                });
                ctx.request_repaint();

                let mut current_device: Option<DeviceId> = None;

                // Forward decoded events to the panel until cancelled.
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        evt = rx.recv() => {
                            match evt {
                                Ok(device::device::DeviceEvent::Log(log)) => {
                                    // Serial panel view: always forward so
                                    // pre-Awake logs are visible in the
                                    // USART2 Logs sub-pane even before a
                                    // device has been bound.
                                    if let device::logs::LogContents::Structured(ref entry) =
                                        log.contents
                                    {
                                        let _ = tx.send(Event::SerialStructuredLog {
                                            index,
                                            entry: entry.clone(),
                                        });
                                    }
                                    // Device log viewer: route to the bound
                                    // device once one exists.
                                    if let Some(dev_id) = current_device {
                                        let _ = tx.send(Event::Log {
                                            device: dev_id,
                                            log,
                                        });
                                    }
                                    ctx.request_repaint();
                                }
                                Ok(device::device::DeviceEvent::Control { event, .. }) => {
                                    // Awake is the device-attached signal on
                                    // USART2. Bind (or re-bind on reboot) the
                                    // persistent device before forwarding.
                                    if let device::logs::ControlEvent::Awake {
                                        uid,
                                        firmware_id,
                                        ..
                                    } = &event
                                    {
                                        let chip_uid = ChipUid(*uid);

                                        // On reboot, remove the old device entry.
                                        if let Some(old_id) = current_device {
                                            bridge_devices.lock().unwrap().remove(&old_id);
                                        }

                                        handle_usart2_awake(
                                            index,
                                            adapter_id,
                                            &port,
                                            chip_uid,
                                            *firmware_id,
                                            &mut current_device,
                                            &tx,
                                            &persistent_devices,
                                            &next_device_id,
                                            &ctx,
                                        );

                                        // Create the PhysicalDevice now that we
                                        // know what's on the other end. Attach
                                        // the adapter so its SerialIpc capability
                                        // is registered, then insert into the
                                        // bridge map so IPC dispatch can find it.
                                        if let Some(dev_id) = current_device {
                                            if let Some(usart2) = adapter.take() {
                                                let mut dev = device::physical::PhysicalDevice::new();
                                                dev.attach(usart2);
                                                bridge_devices.lock().unwrap().insert(
                                                    dev_id,
                                                    BridgeDevice {
                                                        device: Box::new(dev),
                                                        cancel: cancel.clone(),
                                                    },
                                                );
                                            }
                                        }
                                    }

                                    let _ = tx.send(Event::SerialControlEvent { index, event });
                                    ctx.request_repaint();
                                }
                                Ok(_) => {}
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    }
                }

                // Port loop ended — remove device from bridge map.
                if let Some(dev_id) = current_device.take() {
                    bridge_devices.lock().unwrap().remove(&dev_id);
                }
                // Clear the sender slot so a subsequent `ProbeMoshiMoshi`
                // doesn't try to write to a dead wire.
                *usart2_sender_slot.lock().unwrap() = None;
                let _ = tx.send(Event::AdapterRemoved { adapter_id });
                ctx.request_repaint();
                return;
            }
            Err(_) => {
                let _ = tx.send(Event::SerialStatus {
                    index,
                    status: SerialPortStatus::Error,
                });
                ctx.request_repaint();
            }
        }

        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
        }
    }
}

/// Handle an `Awake` control event on a USART2 adapter. Looks up or
/// mints a persistent device for `uid`, emits the binding events, and
/// updates `current_device`.
///
/// If `current_device` was already set (device rebooted mid-session),
/// the adapter is torn down and re-announced first — same shape as the
/// USART1 SFBL re-bind.
#[allow(clippy::too_many_arguments)]
fn handle_usart2_awake(
    index: SerialPortIndex,
    adapter_id: device::adapter::AdapterId,
    port: &str,
    uid: ChipUid,
    build_id_bytes: [u8; 16],
    current_device: &mut Option<DeviceId>,
    tx: &crossbeam_channel::Sender<Event>,
    persistent_devices: &Arc<Mutex<HashMap<ChipUid, DeviceId>>>,
    next_device_id: &Arc<AtomicU64>,
    ctx: &egui::Context,
) {
    // Reboot during session: unbind from the old device. The adapter
    // persists — only the binding changes.
    if let Some(old_id) = current_device.take() {
        eprintln!("[usart2:{port}] awake during session — device rebooted, re-binding");
        let _ = tx.send(Event::AdapterUnbound {
            adapter_id,
            device_id: old_id,
        });
    }

    // Get-or-create the persistent DeviceId for this chip UID.
    let persistent_id = get_or_create_persistent_device(
        uid,
        format!("Device {uid}"),
        None,
        persistent_devices,
        next_device_id,
        tx,
    );

    eprintln!("[usart2:{port}] discovery: identified device {uid} ({persistent_id:?})");

    let _ = tx.send(Event::AdapterBound {
        adapter_id,
        device_id: persistent_id,
        capabilities: vec![KnownCapability::Ipc],
    });
    // Awake also carries the firmware image's build id. Forward it so
    // state.rs can resolve it against the loaded .tfw archives and
    // bind firmware metadata to the device.
    let _ = tx.send(Event::DeviceReportedBuildId {
        device_id: persistent_id,
        build_id_bytes,
    });
    // Awake fires from sysmodule_log::main, which only runs once the
    // kernel has started all tasks — so the device is unambiguously
    // past bootloader by the time we see this.
    let _ = tx.send(Event::DevicePhaseChanged {
        device_id: persistent_id,
        phase: DevicePhase::Kernel,
    });
    let _ = tx.send(Event::SerialStatus {
        index,
        status: SerialPortStatus::DeviceDetected,
    });

    *current_device = Some(persistent_id);
    ctx.request_repaint();
}

// ── Stub flash via SifliDebug ───────────────────────────────────────────

/// One contiguous region to write to device RAM.
///
/// `flash_stub_via_debug` collates every data segment + BSS zero-fill from
/// the stub image into a flat list of these, sorts them smallest-first,
/// then writes them in that order. Smallest-first lets the smaller writes
/// finish quickly so the bar moves visibly before the long tail of the
/// largest write.
struct PendingWrite {
    addr: u32,
    data: Vec<u32>,
}

/// Wire bytes of overhead per `MemWrite` command, independent of payload.
///
/// Breakdown:
/// - frame start marker (2)
/// - frame length field (2)
/// - frame channel + crc (2)
/// - MemWrite opcode (2)
/// - destination address (4)
/// - word count (2)
///
/// Total: 14 bytes on the wire before the first data byte of the chunk.
const MEM_WRITE_WIRE_OVERHEAD: u64 = 14;

/// Perform a single `mem_write` with a byte-counter-driven progress bar.
///
/// Runs the write and a periodic sampler in the same task via `select!`.
/// Every 50 ms the sampler reads the underlying writer's byte counter and
/// emits a `FlashProgress` event. When the write completes, we take the
/// completion branch, do one last sample (with the counter now at its
/// final value for this chunk), and emit the authoritative final event.
///
/// For each sample:
///
/// ```text
/// wire_delta    = counter - counter_at_chunk_start
/// payload_bytes = max(0, wire_delta - MEM_WRITE_WIRE_OVERHEAD)
///                 clamped to chunk_payload_bytes
/// ```
///
/// The header overhead is subtracted so the bar reflects actual user
/// data transmitted, not framing overhead.
async fn write_chunk_with_progress(
    session: &serial::DebugSession,
    addr: u32,
    chunk: &[u32],
    completed_payload_before: u32,
    bytes_total: u32,
    device_id: DeviceId,
    tx: &crossbeam_channel::Sender<Event>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let counter = session.byte_counter();
    let chunk_start = counter.load(Ordering::Relaxed);
    let chunk_payload_bytes = (chunk.len() * 4) as u32;

    let emit = |wire_delta: u64| {
        let payload_this_chunk = wire_delta
            .saturating_sub(MEM_WRITE_WIRE_OVERHEAD)
            .min(chunk_payload_bytes as u64) as u32;
        let bytes_written = completed_payload_before + payload_this_chunk;
        let _ = tx.send(Event::FlashProgress {
            device_id,
            phase: FlashPhase::WritingStub {
                bytes_written,
                bytes_total,
            },
        });
        ctx.request_repaint();
    };

    let write = session.mem_write(addr, chunk);
    tokio::pin!(write);

    loop {
        tokio::select! {
            result = &mut write => {
                // Final authoritative sample — counter is fully up-to-date
                // since mem_write has completed (including flush).
                let wire_delta = counter.load(Ordering::Relaxed).saturating_sub(chunk_start);
                emit(wire_delta);
                return result.map_err(|e| format!("mem_write at {addr:#x}: {e}"));
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                let wire_delta = counter.load(Ordering::Relaxed).saturating_sub(chunk_start);
                emit(wire_delta);
            }
        }
    }
}

/// Write the embedded stub firmware to device RAM via SifliDebug,
/// set VTOR to the stub's entry point, and soft reset.
async fn flash_stub_via_debug(
    session: &serial::DebugSession,
    tx: &crossbeam_channel::Sender<Event>,
    device_id: DeviceId,
    ctx: &egui::Context,
) -> Result<(), String> {
    use std::io::{Cursor, Read};
    use zip::ZipArchive;

    let stub_bytes = crate::stub::TFW;
    let mut archive = ZipArchive::new(Cursor::new(stub_bytes))
        .map_err(|e| format!("invalid stub archive: {e}"))?;

    // Read places.bin from the stub archive.
    let places_data = {
        let mut entry = archive
            .by_name("places.bin")
            .map_err(|e| format!("stub missing places.bin: {e}"))?;
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| format!("read places.bin: {e}"))?;
        buf
    };

    let image = rcard_places::PlacesImage::parse(&places_data)
        .map_err(|e| format!("invalid places.bin: {e:?}"))?;

    let entry_point = image.entry_point();

    // Collate every region we need to write into a flat list, then sort
    // smallest-first. Smallest-first lets quick wins (BSS zero-fills, tiny
    // segments) finish early so the bar visibly moves before the long
    // tail of the largest write.
    let mut writes: Vec<PendingWrite> = Vec::new();
    for seg in image.segments() {
        // Pad data to 4-byte alignment.
        let mut padded = seg.data().to_vec();
        while padded.len() % 4 != 0 {
            padded.push(0);
        }
        let words: Vec<u32> = padded
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        writes.push(PendingWrite {
            addr: seg.dest(),
            data: words,
        });

        if seg.zero_fill() > 0 {
            let n_words = (seg.zero_fill() as usize + 3) / 4;
            writes.push(PendingWrite {
                addr: seg.dest() + seg.file_size(),
                data: vec![0u32; n_words],
            });
        }
    }
    writes.sort_by_key(|w| w.data.len());

    let bytes_total: u32 = writes.iter().map(|w| (w.data.len() * 4) as u32).sum();
    eprintln!(
        "[flash] {} write(s) totaling {bytes_total} bytes:",
        writes.len()
    );
    for w in &writes {
        eprintln!("[flash]   addr={:#x} bytes={}", w.addr, w.data.len() * 4);
    }

    let _ = tx.send(Event::FlashProgress {
        device_id,
        phase: FlashPhase::WritingStub {
            bytes_written: 0,
            bytes_total,
        },
    });
    ctx.request_repaint();

    // Halt the core before writing.
    // DHCSR: DBGKEY (0xA05F) | C_DEBUGEN | C_HALT
    session
        .mem_write(0xE000_EDF0, &[0xA05F_0003])
        .await
        .map_err(|e| format!("halt core: {e}"))?;

    // Chunk size is deliberately large so the host→device transmission
    // time dominates the per-chunk ACK roundtrip. The progress bar is
    // driven by a byte counter on the underlying writer rather than by
    // chunk completion, so a big chunk still shows smooth progress.
    let chunk_size: usize = 262144; // 1 MiB per write (words)
    let mut bytes_written: u32 = 0;
    for write in &writes {
        for (i, chunk) in write.data.chunks(chunk_size).enumerate() {
            let addr = write.addr + (i * chunk_size * 4) as u32;
            write_chunk_with_progress(
                session,
                addr,
                chunk,
                bytes_written,
                bytes_total,
                device_id,
                tx,
                ctx,
            )
            .await?;
            bytes_written += (chunk.len() * 4) as u32;
        }
    }

    // Verify every byte we just wrote by reading it back over the wire
    // and diffing. This catches RAM bit flips, partial writes, and any
    // protocol-level corruption between host and device. Reads in
    // 1024-word (4 KiB) chunks to stay well under any per-frame size
    // limit on the SifliDebug protocol.
    eprintln!("[flash] verifying {} write(s)...", writes.len());
    let mut bytes_verified: u32 = 0;
    let _ = tx.send(Event::FlashProgress {
        device_id,
        phase: FlashPhase::VerifyingStub {
            bytes_verified: 0,
            bytes_total,
        },
    });
    ctx.request_repaint();
    for (write_idx, write) in writes.iter().enumerate() {
        const VERIFY_CHUNK_WORDS: usize = 1024;
        let mut word_offset = 0usize;
        while word_offset < write.data.len() {
            let want =
                &write.data[word_offset..(word_offset + VERIFY_CHUNK_WORDS).min(write.data.len())];
            let addr = write.addr + (word_offset * 4) as u32;
            let got = session
                .mem_read(addr, want.len() as u16)
                .await
                .map_err(|e| format!("verify mem_read at {addr:#x}: {e}"))?;
            if got.len() != want.len() {
                return Err(format!(
                    "verify: addr={addr:#x} length mismatch: expected {} words, got {}",
                    want.len(),
                    got.len()
                ));
            }
            for (i, (&w, &g)) in want.iter().zip(got.iter()).enumerate() {
                if w != g {
                    let bad_addr = addr + (i * 4) as u32;
                    eprintln!(
                        "[flash] VERIFY MISMATCH write {} addr {:#x}: expected {:#010x} got {:#010x}",
                        write_idx, bad_addr, w, g
                    );
                    let lo = i.saturating_sub(4);
                    let hi = (i + 4).min(want.len());
                    for j in lo..hi {
                        let mark = if j == i { "  <-- HERE" } else { "" };
                        eprintln!(
                            "[flash]   {:#010x}: want {:#010x}  got {:#010x}{}",
                            addr + (j * 4) as u32,
                            want[j],
                            got[j],
                            mark
                        );
                    }
                    return Err(format!(
                        "verify failed at {bad_addr:#x}: expected {w:#010x} got {g:#010x}"
                    ));
                }
            }
            bytes_verified += (want.len() * 4) as u32;
            let _ = tx.send(Event::FlashProgress {
                device_id,
                phase: FlashPhase::VerifyingStub {
                    bytes_verified,
                    bytes_total,
                },
            });
            ctx.request_repaint();
            word_offset += VERIFY_CHUNK_WORDS;
        }
    }
    eprintln!("[flash] verify OK ({} bytes)", bytes_total);

    // All RAM writes done — transition the modal to "Booting" before we
    // start poking debug registers (SP, PC, resume).
    let _ = tx.send(Event::FlashProgress {
        device_id,
        phase: FlashPhase::BootingStub,
    });
    ctx.request_repaint();

    // Pull the vector table out of the in-memory image — we just wrote
    // these bytes ourselves, no point reading them back over the wire.
    // The entry point may be at any offset inside one of the writes, not
    // necessarily at the start.
    let (initial_sp, reset_vector) = {
        let entry_write = writes
            .iter()
            .find(|w| {
                let start = w.addr;
                let end = w.addr.wrapping_add((w.data.len() * 4) as u32);
                entry_point >= start && entry_point < end
            })
            .ok_or_else(|| format!("no segment contains entry point {entry_point:#x}"))?;
        let byte_offset = (entry_point - entry_write.addr) as usize;
        if byte_offset % 4 != 0 {
            return Err(format!(
                "entry point {entry_point:#x} not 4-byte aligned within segment"
            ));
        }
        let word_offset = byte_offset / 4;
        if word_offset + 2 > entry_write.data.len() {
            return Err(format!(
                "segment at {:#x} too short for vector table at {entry_point:#x}",
                entry_write.addr
            ));
        }
        (
            entry_write.data[word_offset],
            entry_write.data[word_offset + 1],
        )
    };

    // Write SP (register 13) via debug registers.
    // DCRDR = value, DCRSR = REGWnR (bit 16) | register number
    session
        .mem_write(0xE000_EDF8, &[initial_sp])
        .await
        .map_err(|e| format!("write SP value: {e}"))?;
    session
        .mem_write(0xE000_EDF4, &[0x0001_000D])
        .await
        .map_err(|e| format!("write SP select: {e}"))?;

    // Write PC (register 15).
    session
        .mem_write(0xE000_EDF8, &[reset_vector])
        .await
        .map_err(|e| format!("write PC value: {e}"))?;
    session
        .mem_write(0xE000_EDF4, &[0x0001_000F])
        .await
        .map_err(|e| format!("write PC select: {e}"))?;

    // Resume execution.
    // DHCSR: DBGKEY | C_DEBUGEN (clear C_HALT)
    session
        .mem_write(0xE000_EDF0, &[0xA05F_0001])
        .await
        .map_err(|e| format!("resume core: {e}"))?;

    Ok(())
}

// ── eFuse reader ───────────────────────────────────────────────────────

/// Parse a single ASCII hex digit — returns `None` for any non-hex byte.
/// Parse the `T<16 hex digits> ` tick prefix emitted by the supervisor.
/// Returns `(Some(tick), stripped_text)` on success, or `(None, original)` if
/// the line doesn't carry a tick prefix (e.g. early boot before the kernel
/// timer starts).
fn parse_tick_prefix(line: &str) -> (Option<u64>, String) {
    let bytes = line.as_bytes();
    // 'T' + 16 hex + ' ' = 18 bytes minimum
    if bytes.len() >= 18 && bytes[0] == b'T' && bytes[17] == b' ' {
        let mut tick: u64 = 0;
        for &b in &bytes[1..17] {
            let nib = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return (None, line.to_string()),
            };
            tick = (tick << 4) | nib as u64;
        }
        (Some(tick), line[18..].to_string())
    } else {
        (None, line.to_string())
    }
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Decode 32 ASCII hex chars into 16 bytes, lowercase or uppercase.
fn parse_hex16(s: &[u8]) -> Option<[u8; 16]> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = (hex_nibble(s[i * 2])? << 4) | hex_nibble(s[i * 2 + 1])?;
    }
    Some(out)
}

/// Parse a `hello uid=<32hex> build=<32hex>` line as emitted by the
/// supervisor's `OP_EMIT_LOG` handler in response to a USART2 MoshiMoshi.
/// Returns `(uid, firmware_id)` on a successful match.
fn parse_hello_line(line: &str) -> Option<([u8; 16], [u8; 16])> {
    let rest = line.strip_prefix("hello uid=")?;
    if rest.len() < 32 {
        return None;
    }
    let uid_hex = &rest.as_bytes()[..32];
    let rest = &rest[32..];
    let build_hex = rest.strip_prefix(" build=")?;
    if build_hex.len() != 32 {
        return None;
    }
    let uid = parse_hex16(uid_hex)?;
    let build = parse_hex16(build_hex.as_bytes())?;
    Some((uid, build))
}

/// Read the 128-bit chip UID from eFuse bank 0 via the EFUSEC controller.
///
/// Sequence: set CR (bank=0, mode=read), trigger EN, poll SR.DONE,
/// read BANK0_DATA0..3.
async fn efuse_read_uid(session: &serial::DebugSession) -> Result<ChipUid, String> {
    const EFUSEC: u32 = 0x5000_C000;
    const CR: u32 = EFUSEC + 0x00;
    const SR: u32 = EFUSEC + 0x08;
    const BANK0_DATA0: u32 = EFUSEC + 0x30;

    // CR: BANKSEL=0 (bits [3:2]), MODE=0 (bit [1]) = read.
    session
        .mem_write(CR, &[0x00])
        .await
        .map_err(|e| format!("CR write: {e}"))?;
    // Trigger: EN=1 (bit [0]), self-clearing.
    session
        .mem_write(CR, &[0x01])
        .await
        .map_err(|e| format!("EN write: {e}"))?;

    // Poll SR.DONE (bit [0]).
    for _ in 0..100 {
        let sr = session
            .mem_read(SR, 1)
            .await
            .map_err(|e| format!("SR read: {e}"))?;
        if sr.first().is_some_and(|v| v & 1 != 0) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }

    // Read BANK0_DATA0..DATA3 (4 words = 128 bits = UID).
    let words = session
        .mem_read(BANK0_DATA0, 4)
        .await
        .map_err(|e| format!("DATA read: {e}"))?;
    if words.len() < 4 {
        return Err(format!("expected 4 words, got {}", words.len()));
    }

    let mut uid = [0u8; 16];
    for (i, word) in words[..4].iter().enumerate() {
        uid[i * 4..(i + 1) * 4].copy_from_slice(&word.to_le_bytes());
    }

    // Clear DONE.
    let _ = session.mem_write(SR, &[0x01]).await;

    Ok(ChipUid(uid))
}

// ── Discovery handshake ────────────────────────────────────────────────

/// Outcome of a SifliDebug discovery attempt on a USART1 adapter.
enum DiscoveryOutcome {
    /// Enter SifliDebug failed — no device present, or device unresponsive.
    NotAttached,
    /// Enter succeeded but UID read failed — something is there but we
    /// can't identify it.
    Unidentified { session: serial::DebugSession },
    /// Enter and UID read both succeeded.
    Identified {
        session: serial::DebugSession,
        uid: ChipUid,
    },
}

/// Attempt to identify the device on a USART1 connection via SifliDebug.
async fn try_discover_usart1(sifli_debug: &serial::SifliDebug, port: &str) -> DiscoveryOutcome {
    eprintln!("[usart1:{port}] discovery: probing for SifliDebug");
    let t0 = std::time::Instant::now();
    let Some(session) = sifli_debug.try_acquire().await else {
        eprintln!("[usart1:{port}] discovery: no SifliDebug response (no device attached)");
        return DiscoveryOutcome::NotAttached;
    };
    eprintln!(
        "[usart1:{port}] discovery: entered debug in {:?}",
        t0.elapsed()
    );
    match efuse_read_uid(&session).await {
        Ok(uid) => {
            eprintln!("[usart1:{port}] discovery: identified device {uid}");
            DiscoveryOutcome::Identified { session, uid }
        }
        Err(e) => {
            eprintln!("[usart1:{port}] discovery: UID read failed ({e}), keeping as ephemeral");
            DiscoveryOutcome::Unidentified { session }
        }
    }
}

/// Translate a discovery outcome into a newly-registered device.
///
/// Emits the appropriate `DeviceCreated` event and returns the resulting
/// `DeviceId`. The caller becomes the current-device for its adapter.
///
/// The passed-in session is held until this function returns, then dropped
/// (exiting debug mode and letting the chip resume execution).
fn register_from_discovery(
    outcome: DiscoveryOutcome,
    port: &str,
    adapter_id: device::adapter::AdapterId,
    tx: &crossbeam_channel::Sender<Event>,
    persistent_devices: &Arc<Mutex<HashMap<ChipUid, DeviceId>>>,
    next_device_id: &Arc<AtomicU64>,
    ctx: &egui::Context,
) -> Option<DeviceId> {
    match outcome {
        DiscoveryOutcome::NotAttached => None,
        DiscoveryOutcome::Unidentified { session: _session } => {
            let id = DeviceId(next_device_id.fetch_add(1, Ordering::Relaxed));
            let _ = tx.send(Event::DeviceCreated {
                device_id: id,
                name: format!("{} (USART1)", port),
                kind: DeviceKind::Ephemeral,
                firmware_id: None,
            });
            let _ = tx.send(Event::AdapterBound {
                adapter_id,
                device_id: id,
                capabilities: vec![KnownCapability::SifliDebug],
            });
            ctx.request_repaint();
            Some(id)
        }
        DiscoveryOutcome::Identified {
            session: _session,
            uid,
        } => {
            let id = get_or_create_persistent_device(
                uid,
                format!("Device {uid}"),
                None,
                persistent_devices,
                next_device_id,
                tx,
            );
            let _ = tx.send(Event::AdapterBound {
                adapter_id,
                device_id: id,
                capabilities: vec![KnownCapability::SifliDebug],
            });
            ctx.request_repaint();
            Some(id)
        }
    }
}

// ── USB supervisor ──────────────────────────────────────────────────────

/// Per-fob state the supervisor tracks.
struct UsbEntry {
    chip_uid: ChipUid,
    adapter_id: device::adapter::AdapterId,
    device_id: DeviceId,
    /// Receiver for the underlying device's broadcast channel. Drives
    /// `DeviceReportedBuildId` / `Log` forwarding for USB-sourced events.
    _events_task: tokio::task::JoinHandle<()>,
}

/// Owns native-USB lifecycle for all rcard fobs. Consumes the
/// `nusb::watch_devices` hotplug stream (via `usb::watch_fobs`), which
/// self-seeds with already-attached devices at subscription time.
async fn usb_supervisor_loop(
    tx: crossbeam_channel::Sender<Event>,
    ctx: egui::Context,
    bridge_devices: Arc<Mutex<HashMap<DeviceId, BridgeDevice>>>,
    persistent_devices: Arc<Mutex<HashMap<ChipUid, DeviceId>>>,
    next_device_id: Arc<AtomicU64>,
    flash_wait_usb: Arc<Mutex<HashMap<ChipUid, (u64, tokio::sync::oneshot::Sender<()>)>>>,
) {
    use futures_util::StreamExt;

    // Adapter IDs for USB live in a separate numeric range from USART1/2
    // (which use `index` and `index + 500_000`). 1_000_000+ avoids collisions.
    const USB_ADAPTER_ID_BASE: u64 = 1_000_000;
    let mut next_usb_adapter_index: u64 = 0;

    // Two indexes into the same UsbEntry set: one by nusb DeviceId (for
    // O(1) detach lookup), one by ChipUid (so attach-dedup doesn't let
    // us open a second Usb for the same fob if nusb surfaces a spurious
    // Connected event).
    let mut entries_by_nusb: HashMap<nusb::DeviceId, ChipUid> = HashMap::new();
    let mut entries_by_uid: HashMap<ChipUid, UsbEntry> = HashMap::new();

    let mut stream = match usb::watch_fobs() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[usb] hotplug watch failed to start: {e}");
            return;
        }
    };

    while let Some(event) = stream.next().await {
        match event {
            usb::FobEvent::Connected(fob) => {
                let Some(uid) = parse_chip_uid(&fob.serial) else {
                    eprintln!("[usb:{}] malformed serial descriptor, ignoring", fob.serial);
                    continue;
                };
                if entries_by_uid.contains_key(&uid) {
                    // Duplicate event (typical when the seed race and
                    // a real Connected arrive for the same device).
                    continue;
                }

                let adapter_id =
                    device::adapter::AdapterId(USB_ADAPTER_ID_BASE + next_usb_adapter_index);
                next_usb_adapter_index += 1;

                match handle_usb_attach(
                    &fob.serial,
                    uid,
                    adapter_id,
                    &tx,
                    &ctx,
                    &bridge_devices,
                    &persistent_devices,
                    &next_device_id,
                    &flash_wait_usb,
                ) {
                    Ok(entry) => {
                        entries_by_nusb.insert(fob.id, uid);
                        entries_by_uid.insert(uid, entry);
                    }
                    Err(e) => {
                        eprintln!("[usb:{}] attach failed: {e}", fob.serial);
                    }
                }
            }
            usb::FobEvent::Disconnected(nusb_id) => {
                let Some(uid) = entries_by_nusb.remove(&nusb_id) else {
                    continue;
                };
                if let Some(entry) = entries_by_uid.remove(&uid) {
                    handle_usb_detach(entry.chip_uid, &entry, &tx, &ctx, &bridge_devices);
                }
            }
        }
    }
}

/// Parse a chip UID from its USB serial string (32 hex chars, little-endian
/// byte order matching `ChipUid::Display`). Returns `None` on bad input.
fn parse_chip_uid(serial: &str) -> Option<ChipUid> {
    if serial.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, chunk) in serial.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk).ok()?;
        bytes[i] = u8::from_str_radix(s, 16).ok()?;
    }
    Some(ChipUid(bytes))
}

#[allow(clippy::too_many_arguments)]
fn handle_usb_attach(
    serial: &str,
    uid: ChipUid,
    adapter_id: device::adapter::AdapterId,
    tx: &crossbeam_channel::Sender<Event>,
    ctx: &egui::Context,
    bridge_devices: &Arc<Mutex<HashMap<DeviceId, BridgeDevice>>>,
    persistent_devices: &Arc<Mutex<HashMap<ChipUid, DeviceId>>>,
    next_device_id: &Arc<AtomicU64>,
    flash_wait_usb: &Arc<Mutex<HashMap<ChipUid, (u64, tokio::sync::oneshot::Sender<()>)>>>,
) -> Result<UsbEntry, String> {
    // Resolve / mint the persistent DeviceId for this chip UID.
    let device_id = get_or_create_persistent_device(
        uid,
        format!("Device {uid}"),
        None,
        persistent_devices,
        next_device_id,
        tx,
    );

    // Ensure a BridgeDevice exists, grab a LogSink from it.
    let sink = {
        let mut devs = bridge_devices.lock().unwrap();
        let bridge_dev = devs.entry(device_id).or_insert_with(|| {
            let dev = device::physical::PhysicalDevice::new();
            BridgeDevice {
                device: Box::new(dev),
                cancel: tokio_util::sync::CancellationToken::new(),
            }
        });
        bridge_dev
            .device
            .log_sink(adapter_id)
            .ok_or_else(|| "device does not expose a log sink".to_string())?
    };

    let usb = usb::Usb::connect(adapter_id, serial, sink).map_err(|e| format!("{e}"))?;

    // Subscribe to the device's broadcast before attaching so we don't
    // miss the AdapterConnected event. Also used to forward Awake logs
    // → DeviceReportedBuildId in the future.
    let mut rx = {
        let devs = bridge_devices.lock().unwrap();
        let bridge_dev = devs
            .get(&device_id)
            .ok_or_else(|| "bridge device vanished between log_sink() and attach()".to_string())?;
        bridge_dev.device.subscribe()
    };

    // Attach on the bridge device. This registers the Ipc capability and
    // emits AdapterConnected on the broadcast channel.
    {
        let mut devs = bridge_devices.lock().unwrap();
        if let Some(bridge_dev) = devs.get_mut(&device_id) {
            bridge_dev.device.attach_adapter(Box::new(usb));
        }
    }

    // Announce to the GUI.
    let _ = tx.send(Event::AdapterCreated {
        adapter_id,
        display_name: format!("USB ({serial})"),
        ipc_transport: Some(("usb", usb::USB_IPC_PRIORITY)),
    });
    let _ = tx.send(Event::AdapterBound {
        adapter_id,
        device_id,
        capabilities: vec![KnownCapability::Ipc],
    });
    ctx.request_repaint();

    // If a USART1 flash handler is waiting for this UID to come up over
    // USB, fire the oneshot. Remove the entry regardless so a late
    // spurious fire doesn't clobber a future flash attempt.
    if let Some((_gen, sender)) = flash_wait_usb.lock().unwrap().remove(&uid) {
        let _ = sender.send(());
    }

    // Spawn a task to forward device events for future expansion
    // (Awake → DeviceReportedBuildId, etc). Drops cleanly on detach when
    // the broadcast channel closes.
    let forward_tx = tx.clone();
    let forward_ctx = ctx.clone();
    let events_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(device::device::DeviceEvent::Log(log)) => {
                    let _ = forward_tx.send(Event::Log {
                        device: device_id,
                        log,
                    });
                    forward_ctx.request_repaint();
                }
                Ok(device::device::DeviceEvent::AdapterDisconnected(id)) if id == adapter_id => {
                    // This adapter's slot on the device is gone — stop.
                    return;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    eprintln!("[usb:{serial}] attached → {device_id:?}");

    Ok(UsbEntry {
        chip_uid: uid,
        adapter_id,
        device_id,
        _events_task: events_task,
    })
}

fn handle_usb_detach(
    uid: ChipUid,
    entry: &UsbEntry,
    tx: &crossbeam_channel::Sender<Event>,
    ctx: &egui::Context,
    bridge_devices: &Arc<Mutex<HashMap<DeviceId, BridgeDevice>>>,
) {
    {
        let mut devs = bridge_devices.lock().unwrap();
        if let Some(bridge_dev) = devs.get_mut(&entry.device_id) {
            bridge_dev.device.detach_adapter(entry.adapter_id);
        }
    }

    let _ = tx.send(Event::AdapterRemoved {
        adapter_id: entry.adapter_id,
    });
    ctx.request_repaint();

    eprintln!("[usb:{uid}] detached");
}

// ── Build event mapping helpers ─────────────────────────────────────────

fn map_build_state(state: &tfw::build::BuildState) -> PipelinePhase {
    use tfw::build::BuildState::*;
    match state {
        Planning => PipelinePhase::Planning,
        CompilingTasks => PipelinePhase::CompilingTasks,
        Organizing { regions_placed } => PipelinePhase::Organizing {
            regions_placed: *regions_placed,
        },
        CompilingApp => PipelinePhase::CompilingApp,
        ExtractingMetadata => PipelinePhase::ExtractingMetadata,
        Packing => PipelinePhase::Packing,
        Done => PipelinePhase::Done,
    }
}

fn map_crate_kind(kind: tfw::build::CrateKind) -> CrateKind {
    use tfw::build::CrateKind::*;
    match kind {
        Task => CrateKind::Task,
        Kernel => CrateKind::Kernel,
        Bootloader => CrateKind::Bootloader,
    }
}

/// Classify a crate by name and its raw `tfw` kind. Sysmodules don't
/// have their own variant in `tfw::build::CrateKind` — they're tasks
/// whose crate name starts with `sysmodule_`. That convention is the
/// only reliable signal we have today.
fn classify_kind(name: &str, tfw_kind: tfw::build::CrateKind) -> CrateKind {
    if tfw_kind == tfw::build::CrateKind::Task && name.starts_with("sysmodule_") {
        CrateKind::Sysmodule
    } else {
        map_crate_kind(tfw_kind)
    }
}

fn map_crate_state(state: &tfw::build::CrateState) -> CrateBuildState {
    use tfw::build::CrateState::*;
    match state {
        Building => CrateBuildState::Building,
        Compiled => CrateBuildState::Compiled,
        Measuring => CrateBuildState::Measuring,
        Linking => CrateBuildState::Linking,
        Linked => CrateBuildState::Linked,
    }
}

fn map_host_crate_state(state: &tfw::build::HostCrateState) -> HostCrateBuildState {
    use tfw::build::HostCrateState::*;
    match state {
        Building => HostCrateBuildState::Building,
        Running => HostCrateBuildState::Running,
        Done => HostCrateBuildState::Done,
    }
}

/// Forward a raw cargo message as JSON. The frontend decodes and
/// renders it — no parsing happens here.
fn forward_cargo_message(
    tx: &crossbeam_channel::Sender<Event>,
    build_id: BuildId,
    crate_name: &str,
    kind: CrateKind,
    msg: &escargot::Message,
) -> Option<String> {
    let val: serde_json::Value = msg.decode_custom().ok()?;
    let raw = serde_json::to_string(&val).ok()?;
    let _ = tx.send(Event::BuildCrateCargoLine {
        build_id,
        name: crate_name.to_string(),
        kind,
        line: raw.clone(),
    });
    Some(raw)
}
