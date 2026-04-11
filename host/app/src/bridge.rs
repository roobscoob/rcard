use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use device::adapter::Adapter;
use device::device::Device;
use device::logs::Log;
use tokio::sync::mpsc;

use crate::state::{
    BuildId, ChipUid, DeviceId, DeviceKind, DevicePhase, KnownCapability, SerialAdapterType,
    SerialPortIndex, SerialPortStatus,
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
}

// ── Events (bridge → GUI) ──────────────────────────────────────────────

pub enum Event {
    /// An adapter was created by a connection.
    AdapterCreated {
        adapter_id: device::adapter::AdapterId,
        display_name: String,
    },
    /// An adapter was removed.
    AdapterRemoved {
        adapter_id: device::adapter::AdapterId,
    },
    /// A device appeared (from any connection type).
    DeviceCreated {
        device_id: DeviceId,
        name: String,
        kind: DeviceKind,
        adapter_ids: Vec<device::adapter::AdapterId>,
        capabilities: Vec<KnownCapability>,
        /// The firmware this device is running, if known.
        firmware_id: Option<crate::state::FirmwareId>,
    },
    /// A device was removed.
    DeviceDeleted { device_id: DeviceId },
    /// A log from a device.
    Log { device: DeviceId, log: Log },
    /// Build stage progress.
    BuildStage {
        build_id: BuildId,
        stage: String,
        detail: String,
    },
    /// Build log line.
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
}

/// Phase of a flash operation, driven by the bridge.
pub enum FlashPhase {
    /// Attempting to reset the device via SifliDebug.
    Resetting,
    /// Auto-reset failed — asking the user to reset manually.
    WaitingForReset,
    /// Writing stub firmware to RAM.
    Writing { bytes_written: u32, bytes_total: u32 },
    /// Stub loaded, device is booting.
    Booting,
    /// Flash finished successfully.
    Done,
    /// Flash failed.
    Failed(String),
}

// ── Bridge state ───────────────────────────────────────────────────────

/// A device owned by the bridge. The device is a trait object —
/// could be an EmulatedDevice, a PhysicalDevice behind a serial
/// connection, etc.
struct BridgeDevice {
    _device: Box<dyn Device>,
    cancel: tokio_util::sync::CancellationToken,
    _event_task: tokio::task::JoinHandle<()>,
}

struct BridgeSerial {
    cancel: tokio_util::sync::CancellationToken,
    _task: tokio::task::JoinHandle<()>,
}

// ── Main loop ──────────────────────────────────────────────────────────

pub async fn run(
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    event_tx: crossbeam_channel::Sender<Event>,
    ctx: egui::Context,
) {
    let mut devices: HashMap<DeviceId, BridgeDevice> = HashMap::new();
    let mut serials: HashMap<SerialPortIndex, BridgeSerial> = HashMap::new();

    // Shared persistent device registry: UID → DeviceId.
    // Shared device ID counter — all device IDs come from here to avoid collisions.
    let next_device_id: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let persistent_devices: Arc<Mutex<HashMap<ChipUid, DeviceId>>> =
        Arc::new(Mutex::new(HashMap::new()));
    // Pending flash: device_id → tfw_path. Checked by USART1 loop after bootrom entry.
    let pending_flash: Arc<Mutex<HashMap<DeviceId, PathBuf>>> =
        Arc::new(Mutex::new(HashMap::new()));
    // Notified when a new flash request lands — wakes the USART1 loop
    // so it can try to enter debug immediately (without waiting for reset).
    let flash_notify: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());

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

                let task = tokio::task::spawn_blocking({
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
                                    });
                                }
                                let _ = tx.send(Event::DeviceCreated {
                                    device_id,
                                    name: "Emulator".into(),
                                    kind: DeviceKind::Emulator,
                                    adapter_ids,
                                    capabilities: vec![],
                                    firmware_id: Some(firmware_id),
                                });
                                repaint.request_repaint();

                                // Forward events until cancelled or closed.
                                let mut rx = dev.subscribe();
                                loop {
                                    // Can't use tokio::select in spawn_blocking,
                                    // so poll with a timeout.
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

                                // Keep dev alive (owns Renode process) until loop exits.
                                drop(dev);
                                for id in [AdapterId(0), AdapterId(1), AdapterId(2)] {
                                    let _ = tx.send(Event::AdapterRemoved { adapter_id: id });
                                }
                                repaint.request_repaint();
                            }
                            Err(e) => {
                                eprintln!("emulator start failed: {e}");
                                // No DeviceCreated was sent, nothing to delete.
                                repaint.request_repaint();
                            }
                        }
                    }
                });

                // Store a placeholder BridgeDevice so we can cancel it.
                // We don't have the Device here (it's inside the blocking task),
                // but we have the cancel token to stop it.
                devices.insert(
                    device_id,
                    BridgeDevice {
                        _device: Box::new(NullDevice),
                        cancel,
                        _event_task: task,
                    },
                );
            }

            Command::RemoveDevice(id) => {
                if let Some(dev) = devices.remove(&id) {
                    dev.cancel.cancel();
                }
            }

            Command::FlashViaSifliDebug {
                device_id,
                firmware_id: _,
                tfw_path,
            } => {
                pending_flash.lock().unwrap().insert(device_id, tfw_path);
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

                let cancel = tokio_util::sync::CancellationToken::new();
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
                ));

                serials.insert(
                    index,
                    BridgeSerial {
                        cancel,
                        _task: task,
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
                        let msg = match &event {
                            tfw::build::BuildEvent::StageStart { stage } => {
                                let name = format!("{stage:?}");
                                let _ = tx.send(Event::BuildStage {
                                    build_id,
                                    stage: name.clone(),
                                    detail: String::new(),
                                });
                                Some(format!("Stage: {name}"))
                            }
                            tfw::build::BuildEvent::PhaseStart { phase } => {
                                Some(format!("  Phase: {phase:?}"))
                            }
                            tfw::build::BuildEvent::TaskCompiling { task, phase } => {
                                let _ = tx.send(Event::BuildStage {
                                    build_id,
                                    stage: "Compile".into(),
                                    detail: format!("{phase:?}: {task}"),
                                });
                                Some(format!("  Compiling {task} ({phase:?})"))
                            }
                            tfw::build::BuildEvent::RegionMeasured { task, region, size } => {
                                Some(format!("  Measured {task}.{region} = {size} bytes"))
                            }
                            tfw::build::BuildEvent::LayoutResolved { total_regions } => {
                                Some(format!("  Layout resolved: {total_regions} regions"))
                            }
                            tfw::build::BuildEvent::ImageLinked { size } => {
                                Some(format!("  Image linked: {size} bytes"))
                            }
                            tfw::build::BuildEvent::Packed { path } => {
                                Some(format!("  Packed: {}", path.display()))
                            }
                            tfw::build::BuildEvent::MemoryMap { .. } => None,
                            tfw::build::BuildEvent::CargoMessage(m) => {
                                m.decode().ok().and_then(|decoded| {
                                    if let escargot::format::Message::CompilerMessage(cm) = decoded
                                    {
                                        cm.message.rendered.map(|r| r.trim().to_string())
                                    } else {
                                        None
                                    }
                                })
                            }
                            tfw::build::BuildEvent::Done => Some("Build complete.".into()),
                        };
                        if let Some(message) = msg {
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
    pending_flash: Arc<Mutex<HashMap<DeviceId, PathBuf>>>,
    flash_notify: Arc<tokio::sync::Notify>,
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
            )
            .await;
        }
        SerialAdapterType::Usart2 => {
            usart2_connect_loop(index, port, tx, ctx, cancel).await;
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
    pending_flash: Arc<Mutex<HashMap<DeviceId, PathBuf>>>,
    flash_notify: Arc<tokio::sync::Notify>,
) {
    let adapter_id = device::adapter::AdapterId(index as u64);

    loop {
        let _ = tx.send(Event::SerialStatus {
            index,
            status: SerialPortStatus::Connecting,
        });
        ctx.request_repaint();

        let mut conn = match serial::Usart1Connection::open(&port) {
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

        let _ = tx.send(Event::AdapterCreated {
            adapter_id,
            display_name: format!("USART1 ({})", port),
        });
        let _ = tx.send(Event::SerialStatus {
            index,
            status: SerialPortStatus::PortOpen,
        });
        ctx.request_repaint();

        // Current device attached to this adapter, if any.
        let mut current_device: Option<DeviceId> = None;

        let mut line_buf = String::new();
        loop {
            line_buf.clear();
            tokio::select! {
                _ = cancel.cancelled() => {
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
                    match conn.sifli_debug.try_acquire().await {
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
                            if let Err(e) = conn
                                .sifli_debug
                                .resync_on_sentinel(b"SFBL\n".to_vec())
                                .await
                            {
                                eprintln!("[usart1:{port}] resync_on_sentinel failed: {e}");
                                break;
                            }
                            eprintln!("[usart1:{port}] tap resync complete");
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
                result = conn.read_line(&mut line_buf) => {
                    if result.is_none() {
                        // Port closed / error.
                        break;
                    }

                    let line = line_buf.trim_end_matches('\r').to_string();

                    // ── Sentinel: SFBL — bootrom entered ───────────
                    // `ends_with` rather than equality: after a tap resync,
                    // the first line may have a garbled prefix from the
                    // truncated ACK tail, e.g. "\x06SFBL".
                    if line.ends_with("SFBL") {
                        // Device just (re)booted. Detach adapter from current device.
                        // Persistent devices survive; ephemeral ones auto-clean via 0-adapter rule.
                        current_device.take();
                        let _ = tx.send(Event::AdapterRemoved { adapter_id });
                        // Re-create adapter for this new boot cycle.
                        let _ = tx.send(Event::AdapterCreated {
                            adapter_id,
                            display_name: format!("USART1 ({})", port),
                        });

                        // Create ephemeral device immediately so the UI shows it.
                        let ephemeral_id = DeviceId(next_device_id.fetch_add(1, Ordering::Relaxed));

                        let _ = tx.send(Event::DeviceCreated {
                            device_id: ephemeral_id,
                            name: format!("{} (USART1)", port),
                            kind: DeviceKind::Ephemeral,
                            adapter_ids: vec![adapter_id],
                            capabilities: vec![KnownCapability::SifliDebug],
                            firmware_id: None,
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
                            let mut sfbl_buf = String::new();
                            tokio::select! {
                                _ = cancel.cancelled() => {
                                    let _ = tx.send(Event::AdapterRemoved { adapter_id });
                                    ctx.request_repaint();
                                    return;
                                }
                                _ = tokio::time::sleep(stability_delay) => {
                                    // 1s with no SFBL — power is stable.
                                    break;
                                }
                                result = conn.read_line(&mut sfbl_buf) => {
                                    if result.is_none() {
                                        break; // port closed
                                    }
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
                        if let Some(session) = conn.sifli_debug.try_acquire().await {
                            eprintln!("[usart1:{port}] entered debug in {:?}", t0.elapsed());
                            match efuse_read_uid(&session).await {
                                Ok(uid) => {
                                    eprintln!("[usart1:{port}] identified device: {uid}");

                                    // GetOrCreate persistent device for this UID.
                                    let persistent_id = {
                                        let mut registry = persistent_devices.lock().unwrap();
                                        *registry.entry(uid).or_insert_with(|| {
                                            let id = next_device_id.fetch_add(1, Ordering::Relaxed);
                                            DeviceId(id)
                                        })
                                    };

                                    // Tell GUI: upgrade ephemeral → persistent.
                                    let _ = tx.send(Event::DeviceCreated {
                                        device_id: persistent_id,
                                        name: format!("Device {uid}"),
                                        kind: DeviceKind::Persistent,
                                        adapter_ids: vec![adapter_id],
                                        capabilities: vec![KnownCapability::SifliDebug],
                                        firmware_id: None,
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
                                    let flash_tfw = pending_flash.lock().unwrap().remove(&persistent_id);
                                    if let Some(_tfw_path) = flash_tfw {
                                        eprintln!("[usart1:{port}] pending flash — writing stub");

                                        match flash_stub_via_debug(&session, &tx, persistent_id, &ctx).await {
                                            Ok(()) => {
                                                eprintln!("[usart1:{port}] stub written, booting");
                                                let _ = tx.send(Event::FlashProgress {
                                                    device_id: persistent_id,
                                                    phase: FlashPhase::Booting,
                                                });
                                                ctx.request_repaint();
                                                // Device will reboot into stub.
                                                // TODO: connect via USB and flash real firmware.
                                            }
                                            Err(e) => {
                                                eprintln!("[usart1:{port}] stub flash failed: {e}");
                                                let _ = tx.send(Event::FlashProgress {
                                                    device_id: persistent_id,
                                                    phase: FlashPhase::Failed(e),
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

                    // Forward all lines as text logs.
                    if let Some(dev_id) = current_device {
                        let _ = tx.send(Event::Log {
                            device: dev_id,
                            log: device::logs::Log {
                                adapter: adapter_id,
                                contents: device::logs::LogContents::Text(line),
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
        let _ = tx.send(Event::AdapterRemoved { adapter_id });
        ctx.request_repaint();

        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
        }
    }
}

/// USART2 connection loop — opens the port but does not create devices.
/// Device attachment will happen when USART2 detection is implemented.
async fn usart2_connect_loop(
    index: SerialPortIndex,
    port: String,
    tx: crossbeam_channel::Sender<Event>,
    ctx: egui::Context,
    cancel: tokio_util::sync::CancellationToken,
) {
    let adapter_id = device::adapter::AdapterId(index as u64 + 500_000);

    loop {
        let _ = tx.send(Event::SerialStatus {
            index,
            status: SerialPortStatus::Connecting,
        });
        ctx.request_repaint();

        let connect_result = serial::Usart2::connect(&port, adapter_id, {
            // USART2 needs a LogSink but we don't have a device yet.
            // Create a dummy PhysicalDevice just for the sink.
            let mut dev = device::physical::PhysicalDevice::new();
            dev.log_sink(adapter_id)
        });

        match connect_result {
            Ok(_adapter) => {
                let _ = tx.send(Event::AdapterCreated {
                    adapter_id,
                    display_name: format!("USART2 ({})", port),
                });
                let _ = tx.send(Event::SerialStatus {
                    index,
                    status: SerialPortStatus::PortOpen,
                });
                ctx.request_repaint();

                // Keep alive until cancelled.
                cancel.cancelled().await;
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

// ── Stub flash via SifliDebug ───────────────────────────────────────────

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

    // Calculate total bytes for progress reporting.
    let bytes_total: u32 = image.segments().map(|s| {
        let data_len = (s.data().len() + 3) & !3; // padded to 4-byte alignment
        let bss_len = (s.zero_fill() as usize + 3) & !3;
        (data_len + bss_len) as u32
    }).sum();

    let _ = tx.send(Event::FlashProgress {
        device_id,
        phase: FlashPhase::Writing { bytes_written: 0, bytes_total },
    });
    ctx.request_repaint();

    // Halt the core before writing.
    // DHCSR: DBGKEY (0xA05F) | C_DEBUGEN | C_HALT
    session
        .mem_write(0xE000_EDF0, &[0xA05F_0003])
        .await
        .map_err(|e| format!("halt core: {e}"))?;

    // Write each segment to RAM.
    let chunk_size = 2048; // words per write (~8KB)
    let mut bytes_written: u32 = 0;
    for seg in image.segments() {
        let dest = seg.dest();
        let data = seg.data();

        // Pad to 4-byte alignment.
        let mut padded = data.to_vec();
        while padded.len() % 4 != 0 {
            padded.push(0);
        }

        let words: Vec<u32> = padded
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        for (i, chunk) in words.chunks(chunk_size).enumerate() {
            let addr = dest + (i * chunk_size * 4) as u32;
            session
                .mem_write(addr, chunk)
                .await
                .map_err(|e| format!("mem_write at {addr:#x}: {e}"))?;

            bytes_written += (chunk.len() * 4) as u32;
            let _ = tx.send(Event::FlashProgress {
                device_id,
                phase: FlashPhase::Writing { bytes_written, bytes_total },
            });
            ctx.request_repaint();
        }

        // Zero-fill BSS.
        if seg.zero_fill() > 0 {
            let bss_addr = dest + seg.file_size();
            let zero_words = vec![0u32; (seg.zero_fill() as usize + 3) / 4];
            for (i, chunk) in zero_words.chunks(chunk_size).enumerate() {
                let addr = bss_addr + (i * chunk_size * 4) as u32;
                session
                    .mem_write(addr, chunk)
                    .await
                    .map_err(|e| format!("bss zero at {addr:#x}: {e}"))?;

                bytes_written += (chunk.len() * 4) as u32;
                let _ = tx.send(Event::FlashProgress {
                    device_id,
                    phase: FlashPhase::Writing { bytes_written, bytes_total },
                });
                ctx.request_repaint();
            }
        }
    }

    // Read the stub's vector table: [initial_sp, reset_vector].
    let vtor = session
        .mem_read(entry_point, 2)
        .await
        .map_err(|e| format!("read vector table: {e}"))?;
    let initial_sp = vtor[0];
    let reset_vector = vtor[1];

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

// ── Null device placeholder ────────────────────────────────────────────

/// Placeholder for BridgeDevice when the real device lives inside a
/// spawned blocking task (e.g. emulator). The bridge only needs the
/// cancel token to stop it.
struct NullDevice;

impl Device for NullDevice {
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<device::device::DeviceEvent> {
        let (tx, rx) = tokio::sync::broadcast::channel(1);
        drop(tx);
        rx
    }
    fn query_capability(
        &self,
        _: std::any::TypeId,
    ) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
        None
    }
    fn query_all_capabilities(
        &self,
        _: std::any::TypeId,
    ) -> Vec<(
        device::adapter::AdapterId,
        std::sync::Arc<dyn std::any::Any + Send + Sync>,
    )> {
        vec![]
    }
    fn has_capability(&self, _: std::any::TypeId) -> bool {
        false
    }
    fn adapters(&self) -> Vec<(device::adapter::AdapterId, &dyn device::adapter::Adapter)> {
        vec![]
    }
}
