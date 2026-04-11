use std::path::PathBuf;
use std::time::Instant;

use tfw::build::{BuildEvent, CompilePhase, Stage};

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

    while let Some(event) = rx.recv().await {
        match event {
            BuildEvent::StageStart { stage } => {
                stage_start = Instant::now();
                let (icon, label) = match stage {
                    Stage::Config => ("", "config"),
                    Stage::Layout => ("", "layout"),
                    Stage::Linker => ("", "linker"),
                    Stage::Codegen => ("", "codegen"),
                    Stage::Compile => ("", "compile"),
                    Stage::LogMetadata => ("", "metadata"),
                    Stage::Link => ("", "link"),
                    Stage::Pack => ("", "pack"),
                };
                eprint!("{BOLD}{WHITE}{icon:>2}{RESET} {label:<12}");
                match stage {
                    Stage::Compile => eprintln!(),
                    _ => {}
                }
            }

            BuildEvent::PhaseStart { phase } => {
                stage_start = Instant::now();
                let label = match phase {
                    CompilePhase::PartialLink => "partial link",
                    CompilePhase::Sizing => "sizing",
                    CompilePhase::FinalLink => "final link",
                    CompilePhase::Kernel => "kernel",
                    CompilePhase::Bootloader => "bootloader",
                };
                eprint!("   {BLUE}{label}{RESET}");
                task_count = 0;
            }

            BuildEvent::TaskCompiling { task, phase } => {
                if task_count == 0 {
                    eprint!(" {DIM}");
                } else {
                    eprint!("{DIM}, ");
                }
                let color = match phase {
                    CompilePhase::Kernel => YELLOW,
                    _ => "",
                };
                eprint!("{color}{task}{RESET}");
                task_count += 1;
            }

            BuildEvent::RegionMeasured { task, region, size } => {
                let color = if task == "kernel" { YELLOW } else { CYAN };
                eprint!(
                    "\n      {color}{task}{RESET}{DIM}.{RESET}{region} {GREEN}{}{RESET}",
                    human_size(size)
                );
            }

            BuildEvent::LayoutResolved { total_regions } => {
                let elapsed = elapsed_str(stage_start);
                eprintln!("\n   {MAGENTA}{total_regions} regions placed{RESET}{elapsed}");
            }

            BuildEvent::ImageLinked { size } => {
                let elapsed = elapsed_str(stage_start);
                eprintln!("{GREEN}{}{RESET}{elapsed}", human_size(size));
            }

            BuildEvent::Packed { path } => {
                let elapsed = elapsed_str(stage_start);
                let filename = path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                eprintln!("{GREEN}{filename}{RESET}{elapsed}");
            }

            BuildEvent::MemoryMap { segments } => {
                super::memmap::render(&segments);
            }

            BuildEvent::CargoMessage(msg) => {
                if let Ok(decoded) = msg.decode() {
                    if let escargot::format::Message::CompilerMessage(cm) = decoded {
                        if let Some(rendered) = cm.message.rendered {
                            eprint!("{rendered}");
                        }
                    }
                }
            }

            BuildEvent::Done => {
                let total = build_start.elapsed().as_secs_f64();
                eprintln!("\n{BOLD}{GREEN}build complete{RESET} {DIM}in {total:.1}s{RESET}");
            }
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
