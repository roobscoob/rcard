use std::collections::HashMap;
use std::sync::mpsc;

use emulator::peripherals::usart::log::{UsartLog, UsartLogKind};
use emulator::DeviceBuilder;
use rcard_log::LogLevel;

mod metadata;
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

// Visible width of the prefix: " LEVEL task_name │ "
//   = 1 + 5 + 1 + task_pad + 1 + 1 + 1 = task_pad + 10
fn prefix_width(task_pad: usize) -> usize {
    task_pad + 10
}

fn level_color(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Panic => "\x1b[5;1;97;41m", // blinking bold white on red bg
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

/// Print a message with line-wrapping. First line is printed inline (after the
/// already-printed prefix). Continuation lines get space-padded prefix + bar.
/// Source location is right-aligned to the terminal edge.
fn print_wrapped(msg: &str, location: Option<&str>, term_width: usize, prefix_width: usize) {
    let content_width = term_width.saturating_sub(prefix_width);
    if content_width == 0 {
        println!("{msg}");
        return;
    }

    let cont_prefix = format!("{:>width$} {DIM}│{RESET} ", "", width = prefix_width - 3);

    // Split on newlines first, then wrap each logical line
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

    // Print first line inline (prefix already printed by caller)
    if let Some(first) = lines.first() {
        if lines.len() == 1 {
            // Single line: right-align location on same line
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

    // Print remaining lines with continuation prefix, location on the last line
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

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut img_path = None;
    let mut log_metadata_path = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--log-metadata" => {
                i += 1;
                log_metadata_path =
                    Some(args.get(i).expect("--log-metadata requires a path").clone());
            }
            _ if img_path.is_none() => {
                img_path = Some(args[i].clone());
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let img_path = img_path.expect("usage: emulator-cli <sdmmc.img> [--log-metadata <path>]");
    let img = std::fs::read(&img_path).expect("failed to read sdmmc image");

    let log_metadata = log_metadata_path.map(|path| {
        let data = std::fs::read_to_string(&path).expect("failed to read log metadata");
        let entries: Vec<LogMetadataFile> =
            serde_json::from_str(&data).expect("failed to parse log metadata");
        entries
    });

    // Derive renode assets path from image: <firmware>/build/sdmmc.img → <firmware>/renode/
    let img_abs = std::fs::canonicalize(&img_path).expect("failed to resolve image path");
    let firmware_dir = img_abs
        .parent() // build/
        .and_then(|p| p.parent()) // firmware/
        .expect("image must be inside firmware/build/");
    let assets = firmware_dir.join("renode");

    let (tx, rx) = mpsc::channel::<UsartLog>();

    let log_thread = std::thread::spawn(move || {
        let term_width = terminal_width();

        // Build lookup: species_hash → species
        let species_lookup: HashMap<u64, &Species> = log_metadata
            .as_ref()
            .map(|entries| {
                let mut map = HashMap::new();
                for entry in entries {
                    for (hash_str, species) in &entry.species {
                        if let Ok(hash) = u64::from_str_radix(hash_str.trim_start_matches("0x"), 16)
                        {
                            map.insert(hash, species);
                        }
                    }
                }
                map
            })
            .unwrap_or_default();

        // Build lookup: type_id hash → display name (for structs, enums, variants)
        let type_names: HashMap<u64, String> = log_metadata
            .as_ref()
            .map(|entries| {
                let mut map = HashMap::new();
                for entry in entries {
                    for (hash_str, value) in &entry.types {
                        let Ok(hash) =
                            u64::from_str_radix(hash_str.trim_start_matches("0x"), 16)
                        else {
                            continue;
                        };
                        // Use "name" field from the metadata entry
                        if let Some(name) = value.get("name").and_then(|n| n.as_str()) {
                            map.insert(hash, name.to_string());
                        }
                    }
                }
                map
            })
            .unwrap_or_default();

        // Build task index → name lookup
        let task_names: Vec<&str> = log_metadata
            .as_ref()
            .and_then(|entries| entries.first())
            .map(|e| e.task_names.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default();

        // Find longest task name (after stripping sysmodule_ prefix) to determine column width
        let task_pad = task_names
            .iter()
            .map(|n| n.strip_prefix("sysmodule_").unwrap_or(n).len())
            .max()
            .unwrap_or(0)
            .max(6); // at least wide enough for "renode" / "usartN"
        let pw = prefix_width(task_pad);

        for log in rx {
            match log.kind {
                UsartLogKind::Line(line) => {
                    println!(
                        "       {WHITE}{:>tp$}{RESET} {DIM}│{RESET} {}",
                        format!("usart{}", log.channel),
                        line,
                        tp = task_pad,
                    );
                }
                UsartLogKind::Renode(line) => {
                    // Parse "HH:MM:SS.FFFF [LEVEL] message"
                    let parsed = line.find(' ').and_then(|ts_end| {
                        let after_ts = line[ts_end..].trim_start();
                        let after_bracket = after_ts.strip_prefix('[')?;
                        let close = after_bracket.find(']')?;
                        let level = &after_bracket[..close];
                        let msg = after_bracket[close + 1..].trim_start();
                        let ll = match level {
                            "ERROR" => "ERROR",
                            "WARNING" => " WARN",
                            "INFO" => " INFO",
                            "DEBUG" => "DEBUG",
                            _ => level,
                        };
                        Some((ll, msg))
                    });
                    if let Some((ll, msg)) = parsed {
                        println!("{DIM} {ll} {:>tp$} │ {msg}{RESET}", "renode", tp = task_pad);
                    } else {
                        println!(
                            "{DIM}       {:>tp$} │ {line}{RESET}",
                            "renode",
                            tp = task_pad
                        );
                    }
                }
                UsartLogKind::Stream(stream) => {
                    let source = stream.metadata.source;
                    let log_species = stream.metadata.log_species;
                    let level = stream.metadata.level;
                    let timestamp = stream.metadata.timestamp;
                    let lc = level_color(level);
                    let ll = level_label(level);
                    let raw_task = task_names.get(source as usize).copied().unwrap_or("?");
                    let (task, task_color) = match raw_task.strip_prefix("sysmodule_") {
                        Some(stripped) => (stripped, ORANGE),
                        None => (raw_task, CYAN),
                    };
                    if let Some(species) = species_lookup.get(&log_species) {
                        let msg = format_species(&species.format, &stream.values, &type_names);
                        let loc = source_location(species);
                        print!(
                            " {lc}{BOLD}{ll}{RESET} {task_color}{task:>tp$}{RESET} {DIM}│{RESET} ",
                            tp = task_pad,
                        );
                        print_wrapped(&msg, loc.as_deref(), term_width, pw);
                    } else {
                        print!(
                            " {lc}{BOLD}{ll}{RESET} {MAGENTA}{:>tp$}{RESET} {DIM}│{RESET} ",
                            format!("usart{}", log.channel),
                            tp = task_pad,
                        );
                        let mut first = true;
                        for value in stream.values {
                            if first {
                                println!("{}", format_value(&value, &type_names));
                                first = false;
                            } else {
                                println!(
                                    "       {:>tp$}   {CYAN}{}{RESET}",
                                    "",
                                    format_value(&value, &type_names),
                                    tp = task_pad,
                                );
                            }
                        }
                        if first {
                            println!(
                                "{DIM}t={} species=0x{:x} src={}{RESET}",
                                timestamp, log_species, source
                            );
                        }
                    }
                }
            }
        }
    });

    let mut device = DeviceBuilder::new()
        .with_logger(tx)
        .with_renode_assets(assets)
        .build()
        .expect("failed to start emulator");

    device
        .map_sdmmc(&img)
        .expect("failed to load sdmmc image");

    match device.run() {
        Ok(()) => println!("emulation finished"),
        Err(e) => eprintln!("emulation error: {:?}", e),
    }

    drop(device);
    let _ = log_thread.join();
}

/// Build the formatted message string from a format string + values receiver.
fn format_species(
    fmt: &str,
    values: &mpsc::Receiver<rcard_log::OwnedValue>,
    type_names: &HashMap<u64, String>,
) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' && chars.peek() == Some(&'}') {
            chars.next();
            match values.recv() {
                Ok(val) => out.push_str(&format_value(&val, type_names)),
                Err(_) => out.push_str("???"),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn format_value(val: &rcard_log::OwnedValue, type_names: &HashMap<u64, String>) -> String {
    use rcard_log::OwnedValue::*;
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
                // Unit variant or empty struct — just show the name
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
    }
}
