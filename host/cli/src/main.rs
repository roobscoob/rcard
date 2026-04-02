use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use engine::Backend;
use engine::logs::LogEntry;
use rcard_log::{LogLevel, OwnedValue};
use zip::ZipArchive;

mod metadata;
mod stacktrace;
use metadata::{LogMetadataFile, Species};

// ANSI color codes
const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";
const WHITE: &str = "\x1b[37m";
const ORANGE: &str = "\x1b[38;5;208m";

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
}

fn parse_backend(spec: &str, tfw: &PathBuf) -> Box<dyn Backend> {
    if spec == "emulator" {
        Box::new(
            emulator::Emulator::start(tfw)
                .unwrap_or_else(|e| panic!("failed to start emulator: {e}")),
        )
    } else if let Some(ports) = spec.strip_prefix("serial:") {
        let (p1, p2) = ports
            .split_once(',')
            .unwrap_or_else(|| panic!("expected serial:PORT1,PORT2, got {spec}"));
        Box::new(
            serial::Serial::connect(p1, p2)
                .unwrap_or_else(|e| panic!("failed to connect serial: {e}")),
        )
    } else {
        panic!("unknown backend: {spec} (expected \"emulator\" or \"serial:PORT1,PORT2\")");
    }
}

struct TfwMetadata {
    species: HashMap<u64, Species>,
    type_names: HashMap<u64, String>,
    task_names: Vec<String>,
    elf_cache: stacktrace::ElfCache,
}

fn load_metadata(tfw: &PathBuf) -> TfwMetadata {
    let tfw_bytes = std::fs::read(tfw).expect("failed to read tfw");
    let mut archive = ZipArchive::new(Cursor::new(tfw_bytes)).expect("invalid tfw archive");

    let mut entry = archive
        .by_name("log-metadata.json")
        .expect("tfw missing log-metadata.json");
    let mut json = String::new();
    entry
        .read_to_string(&mut json)
        .expect("failed to read log-metadata.json");
    drop(entry);

    let entries: Vec<LogMetadataFile> =
        serde_json::from_str(&json).expect("failed to parse log-metadata.json");

    let mut species = HashMap::new();
    let mut type_names = HashMap::new();
    let mut task_names = Vec::new();

    for entry in &entries {
        for (hash_str, sp) in &entry.species {
            if let Ok(hash) = u64::from_str_radix(hash_str.trim_start_matches("0x"), 16) {
                species.insert(hash, sp.clone());
            }
        }
        for (hash_str, value) in &entry.types {
            let Ok(hash) = u64::from_str_radix(hash_str.trim_start_matches("0x"), 16) else {
                continue;
            };
            if let Some(name) = value.get("name").and_then(|n| n.as_str()) {
                type_names.insert(hash, name.to_string());
            }
        }
        if task_names.is_empty() {
            task_names.clone_from(&entry.task_names);
        }
    }

    // Load task ELFs for stack dump resolution
    let mut elf_cache = stacktrace::ElfCache::new();
    let tfw_bytes2 = std::fs::read(tfw).expect("failed to read tfw");
    let mut archive2 = ZipArchive::new(Cursor::new(tfw_bytes2)).expect("invalid tfw archive");
    elf_cache.load_from_archive(&mut archive2);

    TfwMetadata {
        species,
        type_names,
        task_names,
        elf_cache,
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::TailLogs { tfw, backend } => {
            let meta = load_metadata(&tfw);
            let backend = parse_backend(&backend, &tfw);

            println!("{BOLD}Tailing logs from backend for firmware {tfw:?}:{RESET}");

            tail_logs(backend.as_ref(), &meta).await;
        }
    }
}

async fn tail_logs(backend: &dyn Backend, meta: &TfwMetadata) {
    let logs = backend.logs();

    let mut structured_rx = logs.subscribe_structured();
    let mut hypervisor_rx = logs.subscribe_hypervisor();

    // Merge all auxiliary streams into a single channel.
    let (aux_tx, mut aux_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    for name in logs.auxiliary_streams() {
        if let Some(mut rx) = logs.subscribe_auxiliary(name) {
            let name = name.to_string();
            let tx = aux_tx.clone();
            tokio::spawn(async move {
                while let Ok(text) = rx.recv().await {
                    if tx.send((name.clone(), text)).is_err() {
                        break;
                    }
                }
            });
        }
    }
    drop(aux_tx); // so aux_rx closes when all forwarders are done

    let task_pad = meta
        .task_names
        .iter()
        .map(|n| n.strip_prefix("sysmodule_").unwrap_or(n).len())
        .max()
        .unwrap_or(0)
        .max(6);
    let pw = prefix_width(task_pad);

    loop {
        tokio::select! {
            Ok(entry) = structured_rx.recv() => {
                print_structured(&entry, meta, task_pad, pw);
            }
            Ok(line) = hypervisor_rx.recv() => {
                print_text_line("hypervisor", &line.text, task_pad);
            }
            Some((name, text)) = aux_rx.recv() => {
                print_text_line(&name, &text, task_pad);
            }
            else => break,
        }
    }
}

// --- Display ---

fn print_text_line(source: &str, text: &str, task_pad: usize) {
    println!(
        "       {WHITE}{:>tp$}{RESET} {DIM}│{RESET} {text}",
        source,
        tp = task_pad,
    );
}

fn print_structured(entry: &LogEntry, meta: &TfwMetadata, task_pad: usize, pw: usize) {
    let level = entry.level;
    let lc = level_color(level);
    let ll = level_label(level);
    let term_width = terminal_width();

    let raw_task = meta
        .task_names
        .get(entry.source as usize)
        .map(|s| s.as_str())
        .unwrap_or("?");
    let (task, task_color) = match raw_task.strip_prefix("sysmodule_") {
        Some(stripped) => (stripped, ORANGE),
        None => (raw_task, CYAN),
    };

    if let Some(species) = meta.species.get(&entry.log_species) {
        let msg = format_species(&species.format, &entry.values, &meta.type_names);
        let loc = source_location(species);
        print!(
            " {lc}{BOLD}{ll}{RESET} {task_color}{task:>tp$}{RESET} {DIM}│{RESET} ",
            tp = task_pad,
        );
        print_wrapped(&msg, loc.as_deref(), term_width, pw);
    } else {
        let vals: Vec<String> = entry
            .values
            .iter()
            .map(|v| format_value(v, &meta.type_names))
            .collect();
        let body = if vals.is_empty() {
            String::from("?")
        } else {
            vals.join(", ")
        };
        println!(
            " {lc}{BOLD}{ll}{RESET} {MAGENTA}{task:>tp$}{RESET} {DIM}│{RESET} {body} {DIM}(species=0x{:x}){RESET}",
            entry.log_species,
            tp = task_pad,
        );
    }

    // Check for stack dump values and display backtrace inline
    for value in &entry.values {
        if matches!(value, OwnedValue::StackDump { .. }) {
            if let Some(bt) = meta.elf_cache.resolve(raw_task, value) {
                print_backtrace(&bt, task_pad, term_width, pw);
            }
        }
    }
}

fn print_backtrace(bt: &stacktrace::Backtrace, _task_pad: usize, term_width: usize, pw: usize) {
    let cont_prefix = format!("{:>width$} {DIM}│{RESET} ", "", width = pw - 3,);
    let content_width = term_width.saturating_sub(pw);

    println!("{cont_prefix}{DIM}backtrace:{RESET}");

    // skip the first frame, since it's rcade_log::stack_dump::capture() itself
    for frame in bt.frames.iter().skip(1) {
        let inline_tag = if frame.is_inline { " [inline]" } else { "" };
        let loc = match (&frame.file, frame.line) {
            (Some(file), Some(line)) => format!("{file}:{line}"),
            (Some(file), None) => file.clone(),
            _ => String::new(),
        };

        let name = &frame.function;
        let left = format!("  {name}{inline_tag}");
        if !loc.is_empty() {
            let pad = content_width.saturating_sub(left.len() + loc.len() + 1);
            println!("{cont_prefix}{left}{:pad$} {DIM}{loc}{RESET}", "");
        } else {
            println!("{cont_prefix}{left}");
        }
    }
}

fn prefix_width(task_pad: usize) -> usize {
    task_pad + 10
}

fn level_color(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Panic => "\x1b[5;1;97;41m",
        LogLevel::Error => RED,
        LogLevel::Warn => YELLOW,
        LogLevel::Info => GREEN,
        LogLevel::Debug => BLUE,
        LogLevel::Trace => DIM,
    }
}

fn level_label(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Panic => "PANIC",
        LogLevel::Error => "ERROR",
        LogLevel::Warn => " WARN",
        LogLevel::Info => " INFO",
        LogLevel::Debug => "DEBUG",
        LogLevel::Trace => "TRACE",
    }
}

fn terminal_width() -> usize {
    terminal_size::terminal_size()
        .map(|(w, _)| (w.0 as usize).saturating_sub(1))
        .unwrap_or(119)
}

fn source_location(species: &Species) -> Option<String> {
    let file = species.file.as_deref()?;
    let line = species.line?;
    Some(format!("{file}:{line}"))
}

fn print_wrapped(msg: &str, location: Option<&str>, term_width: usize, prefix_width: usize) {
    let content_width = term_width.saturating_sub(prefix_width);
    if content_width == 0 {
        println!("{msg}");
        return;
    }

    let cont_prefix = format!("{:>width$} {DIM}│{RESET} ", "", width = prefix_width - 3);

    let mut lines: Vec<String> = Vec::new();
    for logical_line in msg.split('\n') {
        let chars: Vec<char> = logical_line.chars().collect();
        if chars.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut pos = 0;
        while pos < chars.len() {
            let end = (pos + content_width).min(chars.len());
            lines.push(chars[pos..end].iter().collect());
            pos = end;
        }
    }

    if let Some(first) = lines.first() {
        if lines.len() == 1 {
            let loc_len = location.map(|l| l.len() + 1).unwrap_or(0);
            let vis_len = first.chars().count();
            if let Some(loc) = location {
                if vis_len + loc_len <= content_width {
                    let pad = content_width - vis_len - loc_len;
                    println!("{first}{:pad$} {DIM}{loc}{RESET}", "");
                } else {
                    println!("{first}");
                    let pad = content_width.saturating_sub(loc.len());
                    println!("{cont_prefix}{:pad$}{DIM}{loc}{RESET}", "");
                }
            } else {
                println!("{first}");
            }
            return;
        }
        println!("{first}");
    }

    let last_idx = lines.len() - 1;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if i == last_idx {
            let vis_len = line.chars().count();
            let loc_len = location.map(|l| l.len() + 1).unwrap_or(0);
            if let Some(loc) = location {
                if vis_len + loc_len <= content_width {
                    let pad = content_width - vis_len - loc_len;
                    println!("{cont_prefix}{line}{:pad$} {DIM}{loc}{RESET}", "");
                } else {
                    println!("{cont_prefix}{line}");
                    let pad = content_width.saturating_sub(loc.len());
                    println!("{cont_prefix}{:pad$}{DIM}{loc}{RESET}", "");
                }
            } else {
                println!("{cont_prefix}{line}");
            }
        } else {
            println!("{cont_prefix}{line}");
        }
    }
}

fn format_species(fmt: &str, values: &[OwnedValue], type_names: &HashMap<u64, String>) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    let mut val_iter = values.iter();
    while let Some(c) = chars.next() {
        if c == '{' && chars.peek() == Some(&'}') {
            chars.next();
            match val_iter.next() {
                Some(val) => out.push_str(&format_value(val, type_names)),
                None => out.push_str("???"),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn format_value(val: &OwnedValue, type_names: &HashMap<u64, String>) -> String {
    use OwnedValue::*;
    match val {
        U8(v) => format!("{v}"),
        I8(v) => format!("{v}"),
        U16(v) => format!("{v}"),
        I16(v) => format!("{v}"),
        U32(v) => format!("{v}"),
        I32(v) => format!("{v}"),
        U64(v) => format!("{v}"),
        I64(v) => format!("{v}"),
        U128(v) => format!("{v}"),
        I128(v) => format!("{v}"),
        F32(v) => format!("{v}"),
        F64(v) => format!("{v}"),
        Char(v) => format!("'{v}'"),
        Bool(v) => format!("{v}"),
        Str(v) => v.clone(),
        Unit => "()".into(),
        Array(items) | Slice(items) => {
            let inner: Vec<String> = items.iter().map(|v| format_value(v, type_names)).collect();
            format!("[{}]", inner.join(", "))
        }
        Tuple { type_id, fields } => {
            let inner: Vec<String> = fields.iter().map(|v| format_value(v, type_names)).collect();
            if let Some(name) = type_names.get(type_id) {
                if inner.is_empty() {
                    name.clone()
                } else {
                    format!("{name}({})", inner.join(", "))
                }
            } else {
                format!("({})", inner.join(", "))
            }
        }
        Struct { type_id, fields } => {
            let name = type_names.get(type_id);
            if fields.is_empty() {
                name.cloned().unwrap_or_else(|| "{}".into())
            } else {
                let inner: Vec<String> = fields
                    .iter()
                    .map(|(_, v)| format_value(v, type_names))
                    .collect();
                if let Some(name) = name {
                    format!("{name} {{{}}}", inner.join(", "))
                } else {
                    format!("{{{}}}", inner.join(", "))
                }
            }
        }
        StackDump { sp, stack, .. } => {
            format!(
                "<stack dump: sp=0x{sp:08x}, {len} bytes>",
                len = stack.len()
            )
        }
    }
}
