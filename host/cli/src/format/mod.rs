pub mod stacktrace;

use std::collections::HashMap;

use engine::logs::LogEntry;
use rcard_log::{LogLevel, OwnedValue};

use crate::metadata::Species;
use crate::tfw::TfwMetadata;

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

pub fn prefix_width(task_pad: usize) -> usize {
    task_pad + 10
}

pub fn terminal_width() -> usize {
    terminal_size::terminal_size()
        .map(|(w, _)| (w.0 as usize).saturating_sub(1))
        .unwrap_or(119)
}

pub fn print_text_line(source: &str, text: &str, task_pad: usize) {
    let (source, text) = match text.split_once(": ") {
        Some((prefix, rest)) => (prefix, rest),
        None => (source, text),
    };
    println!(
        "       {WHITE}{:>tp$}{RESET} {DIM}|{RESET} {text}",
        source,
        tp = task_pad,
    );
}

pub fn print_structured(entry: &LogEntry, meta: &TfwMetadata, task_pad: usize, pw: usize) {
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
            " {lc}{BOLD}{ll}{RESET} {task_color}{task:>tp$}{RESET} {DIM}|{RESET} ",
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
            " {lc}{BOLD}{ll}{RESET} {MAGENTA}{task:>tp$}{RESET} {DIM}|{RESET} {body} {DIM}(species=0x{:x}){RESET}",
            entry.log_species,
            tp = task_pad,
        );
    }

    // Check for stack dump values and display backtrace inline
    for value in &entry.values {
        if matches!(value, OwnedValue::StackDump { .. }) {
            if let Some(bt) = meta.elf_cache.resolve(raw_task, value) {
                stacktrace::print_backtrace(&bt, task_pad, term_width, pw);
            }
        }
    }
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

    let cont_prefix = format!("{:>width$} {DIM}|{RESET} ", "", width = prefix_width - 3);

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
