use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod commands;
mod format;
mod metadata;
mod stub;
mod tfw;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Stream logs from a device or emulator.
    TailLogs {
        /// Path to the .tfw firmware archive.
        #[arg(long)]
        tfw: PathBuf,

        /// Backend to use: "emulator" or "serial:PORT1,PORT2".
        #[arg(long)]
        backend: String,
    },

    /// Format (flash) firmware onto a device.
    Format {
        /// Path to the .tfw firmware archive.
        #[arg(long)]
        tfw: PathBuf,

        /// Backend to use: "emulator" or "serial:PORT1,PORT2".
        #[arg(long)]
        backend: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::TailLogs { tfw, backend } => commands::tail_logs::run(tfw, backend).await,
        Command::Format { tfw, backend } => commands::format::run(tfw, backend).await,
    }
}
