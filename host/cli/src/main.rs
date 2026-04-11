use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

mod commands;
mod format;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build firmware from Nickel config.
    Build {
        /// Config name (e.g. "fob"), resolved as <firmware-dir>/<name>.ncl
        config: String,

        /// Board name (e.g. "bentoboard"), resolved as <firmware-dir>/boards/<name>.ncl
        #[arg(long)]
        board: String,

        /// Layout name (e.g. "prod"), resolved as <firmware-dir>/layouts/<name>.ncl
        #[arg(long)]
        layout: String,

        /// Path to firmware/ directory.
        #[arg(long, default_value = "firmware")]
        firmware_dir: PathBuf,

        /// Output .tfw archive path.
        #[arg(long, default_value = "build/output.tfw")]
        out: PathBuf,
    },
}

/// Resolve a short name like "fob" to a path like "fob.ncl" or "boards/bentoboard.ncl".
/// Exits with a helpful error if the file doesn't exist.
fn resolve_ncl(firmware_dir: &Path, subdir: &str, name: &str, kind: &str) -> String {
    let name = name.strip_suffix(".ncl").unwrap_or(name);
    let rel = if subdir.is_empty() {
        format!("{name}.ncl")
    } else {
        format!("{subdir}/{name}.ncl")
    };
    let full = firmware_dir.join(&rel);
    if !full.exists() {
        eprintln!("error: {kind} not found: {}", full.display());
        if !firmware_dir.exists() {
            eprintln!("  (firmware directory '{}' does not exist)", firmware_dir.display());
        } else if !subdir.is_empty() {
            let dir = firmware_dir.join(subdir);
            if dir.is_dir() {
                let mut available: Vec<String> = std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        name.strip_suffix(".ncl").map(|s| s.to_string())
                    })
                    .collect();
                available.sort();
                if !available.is_empty() {
                    eprintln!("  available: {}", available.join(", "));
                }
            }
        }
        std::process::exit(1);
    }
    rel
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Build {
            config,
            board,
            layout,
            firmware_dir,
            out,
        } => {
            let config_path = resolve_ncl(&firmware_dir, "apps", &config, "config");
            let board_path = resolve_ncl(&firmware_dir, "boards", &board, "board");
            let layout_path = resolve_ncl(&firmware_dir, "layouts", &layout, "layout");
            commands::build::run(firmware_dir, config_path, board_path, layout_path, out).await
        }
    }
}
