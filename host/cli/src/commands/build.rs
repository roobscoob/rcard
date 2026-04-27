use std::path::PathBuf;
use std::time::Instant;

use tfw::build::{
    BuildEvent, BuildState, CrateEvent, CrateKind, CrateState,
    HostCrateState, HostCrateEvent, ImageEvent, ImageState,
    MemoryEvent, ResourceUpdate,
};

// ANSI
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";
const YELLOW: &str = "\x1b[33m";
const WHITE: &str = "\x1b[37m";
const RED: &str = "\x1b[31m";

fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

pub async fn run(
    firmware_dir: PathBuf,
    config: String,
    board: String,
    layout: String,
    out: PathBuf,
) {
    let app = config.strip_suffix(".ncl").unwrap_or(&config).to_string();
    let board_short = board
        .rsplit('/')
        .next()
        .and_then(|f| f.strip_suffix(".ncl"))
        .unwrap_or(&board)
        .to_string();

    eprintln!("{BOLD}{WHITE}building{RESET} {CYAN}{app}{RESET} {DIM}({board_short}){RESET}\n");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let handle = tokio::task::spawn_blocking(move || {
        let on_event = move |event: BuildEvent| {
            let _ = tx.send(event);
        };
        tfw::build::build(
            &firmware_dir,
            &config,
            &board,
            &layout,
            &out,
            Some(&on_event),
            None,
        )
    });

    let build_start = Instant::now();
    let mut stage_start = Instant::now();
    let mut task_count = 0usize;
    let mut last_state: Option<CrateState> = None;

    while let Some(event) = rx.recv().await {
        match event {
            // ── Build state transitions ────────────────────────────
            BuildEvent::Build(state) => {
                // End the previous line if we were listing tasks.
                if task_count > 0 {
                    eprintln!("{RESET}");
                    task_count = 0;
                }
                last_state = None;

                stage_start = Instant::now();
                let label = match &state {
                    BuildState::Planning => "plan",
                    BuildState::CompilingTasks => "compile",
                    BuildState::Organizing { .. } => "organize",
                    BuildState::CompilingApp => "compile app",
                    BuildState::ExtractingMetadata => "metadata",
                    BuildState::Packing => "pack",
                    BuildState::Done => {
                        let total = build_start.elapsed().as_secs_f64();
                        eprintln!(
                            "\n{BOLD}{GREEN}build complete{RESET} {DIM}in {total:.1}s{RESET}"
                        );
                        continue;
                    }
                };
                eprint!("{BOLD}{WHITE}  {label:<14}{RESET}");

                match &state {
                    BuildState::Organizing { regions_placed } => {
                        let elapsed = elapsed_str(stage_start);
                        eprintln!("{MAGENTA}{regions_placed} regions placed{RESET}{elapsed}");
                    }
                    BuildState::CompilingTasks | BuildState::CompilingApp => {
                        eprintln!();
                    }
                    _ => {}
                }
            }

            // ── Crate state transitions ────────────────────────────
            BuildEvent::Crate { name, kind, update: ResourceUpdate::State(state) } => {
                let color = match kind {
                    CrateKind::Kernel => YELLOW,
                    CrateKind::Bootloader => BLUE,
                    CrateKind::Task => "",
                };

                match &state {
                    CrateState::Building | CrateState::Measuring | CrateState::Linking => {
                        // Start a new line when the compile step changes.
                        if last_state.as_ref() != Some(&state) {
                            if task_count > 0 {
                                eprintln!("{RESET}");
                            }
                            let step_label = match &state {
                                CrateState::Building => "building",
                                CrateState::Measuring => "measuring",
                                CrateState::Linking => "linking",
                                _ => unreachable!(),
                            };
                            eprint!("   {BLUE}{step_label}{RESET}");
                            task_count = 0;
                            last_state = Some(state.clone());
                        }

                        if task_count == 0 {
                            eprint!(" {DIM}");
                        } else {
                            eprint!("{DIM}, ");
                        }
                        eprint!("{color}{name}{RESET}");
                        task_count += 1;
                    }
                    CrateState::Compiled | CrateState::Linked => {
                        // Implicit in the flow — don't print.
                    }
                }
            }

            // ── Crate events ───────────────────────────────────────
            BuildEvent::Crate { name, kind, update: ResourceUpdate::Event(event) } => {
                let color = match kind {
                    CrateKind::Kernel => YELLOW,
                    CrateKind::Bootloader => BLUE,
                    CrateKind::Task => CYAN,
                };

                match event {
                    CrateEvent::Sized { region, size } => {
                        eprint!(
                            "\n      {color}{name}{RESET}{DIM}.{RESET}{region} {GREEN}{}{RESET}",
                            human_size(size)
                        );
                    }
                    CrateEvent::CargoMessage(msg) => {
                        if let Ok(decoded) = msg.decode() {
                            if let escargot::format::Message::CompilerMessage(cm) = decoded {
                                if let Some(rendered) = cm.message.rendered {
                                    eprint!("{rendered}");
                                }
                            }
                        }
                    }
                    CrateEvent::CargoError(e) => {
                        eprintln!("\n{RED}cargo error: {e}{RESET}");
                    }
                }
            }

            // ── HostCrate state transitions ────────────────────────
            BuildEvent::HostCrate { name, update: ResourceUpdate::State(state) } => {
                match state {
                    HostCrateState::Queued => {}
                    HostCrateState::Building => {
                        eprint!("   {DIM}{name}{RESET}");
                    }
                    HostCrateState::Running => {
                        eprint!("{DIM} running{RESET}");
                    }
                    HostCrateState::Done => {
                        let elapsed = elapsed_str(stage_start);
                        eprintln!("{elapsed}");
                    }
                }
            }

            // ── HostCrate events ───────────────────────────────────
            BuildEvent::HostCrate { name: _, update: ResourceUpdate::Event(event) } => {
                match event {
                    HostCrateEvent::CargoMessage(msg) => {
                        if let Ok(decoded) = msg.decode() {
                            if let escargot::format::Message::CompilerMessage(cm) = decoded {
                                if let Some(rendered) = cm.message.rendered {
                                    eprint!("{rendered}");
                                }
                            }
                        }
                    }
                    HostCrateEvent::CargoError(e) => {
                        eprintln!("\n{RED}cargo error: {e}{RESET}");
                    }
                }
            }

            // ── Memory events ──────────────────────────────────────
            BuildEvent::Memory { place: _, update: ResourceUpdate::Event(_) } => {
                // Memory allocations are accumulated silently.
                // A future TUI can build a memory map visualization from these.
            }
            BuildEvent::Memory { update: ResourceUpdate::State(_), .. } => {}

            // ── Image state transitions ────────────────────────────
            BuildEvent::Image(ResourceUpdate::State(state)) => {
                match state {
                    ImageState::Assembled { size } => {
                        let elapsed = elapsed_str(stage_start);
                        eprint!("{GREEN}{}{RESET}{elapsed}", human_size(size));
                    }
                    ImageState::Archived { path } => {
                        let elapsed = elapsed_str(stage_start);
                        let filename = path
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string());
                        eprintln!(" {GREEN}{filename}{RESET}{elapsed}");
                    }
                }
            }

            // ── Image events ───────────────────────────────────────
            BuildEvent::Image(ResourceUpdate::Event(event)) => {
                match event {
                    ImageEvent::PlaceWritten { place, dest, file_offset, file_size, mem_size } => {
                        eprintln!(
                            "   {DIM}{place}: {dest:#010x} +{file_offset:#x} ({} file, {} mem){RESET}",
                            human_size(file_size as u64),
                            human_size(mem_size as u64),
                        );
                    }
                }
            }

            BuildEvent::ConfigResolved(_) | BuildEvent::IpcMetadata(_) => {}
        }
    }

    match handle.await.unwrap() {
        Ok(_) => {}
        Err(e) => {
            super::build_error::render(&e);
            std::process::exit(1);
        }
    }
}

fn elapsed_str(start: Instant) -> String {
    let secs = start.elapsed().as_secs_f64();
    if secs < 0.1 {
        return String::new();
    }
    format!(" {DIM}{secs:.1}s{RESET}")
}
