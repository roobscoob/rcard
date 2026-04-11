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
    /// Flash failed at the step that was in progress.
    ///
    /// `at_step` is the step index (0 = reset, 1 = writing, 2 = booting) so
    /// the modal can render a red X next to the failed task and leave any
    /// subsequent tasks as "not started".
    Failed { at_step: usize, message: String },
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
            usart2_connect_loop(
                index,
                port,
                tx,
                ctx,
                cancel,
                persistent_devices,
                next_device_id,
            )
            .await;
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
        let serial::Usart1Connection { mut reader, sifli_debug } = conn;

        let _ = tx.send(Event::AdapterCreated {
            adapter_id,
            display_name: format!("USART1 ({})", port),
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
                                let start = line_start
                                    .take()
                                    .unwrap_or_else(std::time::Instant::now);
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

                    let line = raw_line.trim_end_matches('\r').to_string();

                    // Forward the raw line to the serial adapter terminal.
                    let _ = tx.send(Event::SerialRawLine {
                        index,
                        line: line.clone(),
                    });

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
                                                eprintln!("[usart1:{port}] stub written, waiting for USB");
                                                // Wait for the stub's USB device to appear. The
                                                // stub reports serial = hex of the chip UID,
                                                // so we match against that to make sure it's
                                                // the device we just flashed, not a stale peer.
                                                let serial_hex = format!("{uid}");
                                                let found = usb::wait_for_serial(
                                                    &serial_hex,
                                                    std::time::Duration::from_secs(10),
                                                )
                                                .await;
                                                if found {
                                                    eprintln!("[usart1:{port}] stub USB up, flash done");
                                                    let _ = tx.send(Event::FlashProgress {
                                                        device_id: persistent_id,
                                                        phase: FlashPhase::Done,
                                                    });
                                                } else {
                                                    eprintln!("[usart1:{port}] stub USB never appeared");
                                                    let _ = tx.send(Event::FlashProgress {
                                                        device_id: persistent_id,
                                                        phase: FlashPhase::Failed {
                                                            at_step: 2,
                                                            message: "stub USB device did not appear within 10s".into(),
                                                        },
                                                    });
                                                }
                                                ctx.request_repaint();
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

                    // Forward all lines as text logs. The received_at is
                    // the first-byte-of-line timestamp captured by the
                    // parallel reader task, so ordering in the device
                    // viewer reflects real arrival time.
                    if let Some(dev_id) = current_device {
                        let _ = tx.send(Event::Log {
                            device: dev_id,
                            log: device::logs::Log {
                                adapter: adapter_id,
                                contents: device::logs::LogContents::Text(line),
                                received_at: line_received_at,
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
) {
    let adapter_id = device::adapter::AdapterId(index as u64 + 500_000);

    loop {
        let _ = tx.send(Event::SerialStatus {
            index,
            status: SerialPortStatus::Connecting,
        });
        ctx.request_repaint();

        // Create a PhysicalDevice that owns the log sink. We subscribe to
        // its broadcast so structured logs decoded off USART2 can be
        // forwarded to the serial adapter panel.
        let mut dev = device::physical::PhysicalDevice::new();
        let mut rx = dev.subscribe();
        let connect_result = serial::Usart2::connect(&port, adapter_id, dev.log_sink(adapter_id));

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

                // Port loop ended. Ephemeral devices auto-clean via the
                // 0-adapter rule in state.rs; persistent devices go
                // disconnected. Mirrors the USART1 teardown.
                current_device.take();
                drop(dev);
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
    // Reboot during session: detach and re-announce the adapter.
    if current_device.take().is_some() {
        eprintln!("[usart2:{port}] awake during session — device rebooted, re-binding");
        let _ = tx.send(Event::AdapterRemoved { adapter_id });
        let _ = tx.send(Event::AdapterCreated {
            adapter_id,
            display_name: format!("USART2 ({})", port),
        });
    }

    // Get-or-create the persistent DeviceId for this chip UID.
    let persistent_id = {
        let mut registry = persistent_devices.lock().unwrap();
        *registry
            .entry(uid)
            .or_insert_with(|| DeviceId(next_device_id.fetch_add(1, Ordering::Relaxed)))
    };

    eprintln!("[usart2:{port}] awake: bound device {uid} ({persistent_id:?})");

    let _ = tx.send(Event::DeviceCreated {
        device_id: persistent_id,
        name: format!("Device {uid}"),
        kind: DeviceKind::Persistent,
        adapter_ids: vec![adapter_id],
        capabilities: vec![KnownCapability::Ipc],
        firmware_id: None,
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
            phase: FlashPhase::Writing {
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
    eprintln!("[flash] {} write(s) totaling {bytes_total} bytes:", writes.len());
    for w in &writes {
        eprintln!("[flash]   addr={:#x} bytes={}", w.addr, w.data.len() * 4);
    }

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
                session, addr, chunk, bytes_written, bytes_total, device_id, tx, ctx,
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
    for (write_idx, write) in writes.iter().enumerate() {
        const VERIFY_CHUNK_WORDS: usize = 1024;
        let mut word_offset = 0usize;
        while word_offset < write.data.len() {
            let want = &write.data[word_offset
                ..(word_offset + VERIFY_CHUNK_WORDS).min(write.data.len())];
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
                    // Show a window of context.
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
            word_offset += VERIFY_CHUNK_WORDS;
        }
    }
    eprintln!("[flash] verify OK ({} bytes)", bytes_total);

    // All RAM writes done — transition the modal to "Booting" before we
    // start poking debug registers (SP, PC, resume).
    let _ = tx.send(Event::FlashProgress {
        device_id,
        phase: FlashPhase::Booting,
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
async fn try_discover_usart1(
    sifli_debug: &serial::SifliDebug,
    port: &str,
) -> DiscoveryOutcome {
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
                adapter_ids: vec![adapter_id],
                capabilities: vec![KnownCapability::SifliDebug],
                firmware_id: None,
            });
            ctx.request_repaint();
            Some(id)
        }
        DiscoveryOutcome::Identified {
            session: _session,
            uid,
        } => {
            let id = {
                let mut registry = persistent_devices.lock().unwrap();
                *registry.entry(uid).or_insert_with(|| {
                    DeviceId(next_device_id.fetch_add(1, Ordering::Relaxed))
                })
            };
            let _ = tx.send(Event::DeviceCreated {
                device_id: id,
                name: format!("Device {uid}"),
                kind: DeviceKind::Persistent,
                adapter_ids: vec![adapter_id],
                capabilities: vec![KnownCapability::SifliDebug],
                firmware_id: None,
            });
            ctx.request_repaint();
            Some(id)
        }
    }
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
