use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::sync::mpsc;
use std::thread;

use clap::Parser;
use rcard_log::decoder::{Decoder, FeedResult};
use rcard_log::{LogLevel, LogMetadata, OwnedValue};
use serialport::SerialPort;
use zerocopy::TryFromBytes;

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

#[derive(Parser)]
struct Args {
    #[arg(long)]
    usart1: String,
    #[arg(long)]
    usart2: String,
    #[arg(long)]
    log_metadata: Option<String>,
}

// --- Log entry types (mirrors emulator) ---

struct UsartLog {
    channel: u8,
    kind: UsartLogKind,
}

enum UsartLogKind {
    Line(String),
    Stream(LogStream),
}

struct LogStream {
    metadata: LogMetadata,
    values: mpsc::Receiver<OwnedValue>,
}

// --- StructuredSink (ported from emulator) ---

const METADATA_SIZE: usize = core::mem::size_of::<LogMetadata>();

enum FrameState {
    ReadingId { buf: [u8; 8], pos: u8 },
    ReadingLength { id: u64 },
    ReadingData { id: u64, remaining: u8 },
}

struct StreamState {
    meta_buf: [u8; METADATA_SIZE],
    meta_pos: usize,
    decoder: Decoder,
    tx: Option<mpsc::Sender<OwnedValue>>,
    channel: u8,
    entry_tx: mpsc::Sender<UsartLog>,
}

impl StreamState {
    fn new(channel: u8, entry_tx: mpsc::Sender<UsartLog>) -> Self {
        StreamState {
            meta_buf: [0; METADATA_SIZE],
            meta_pos: 0,
            decoder: Decoder::new(),
            tx: None,
            channel,
            entry_tx,
        }
    }

    fn feed_byte(&mut self, byte: u8) -> bool {
        if self.meta_pos < METADATA_SIZE {
            self.meta_buf[self.meta_pos] = byte;
            self.meta_pos += 1;

            if self.meta_pos == METADATA_SIZE {
                match LogMetadata::try_read_from_bytes(&self.meta_buf) {
                    Ok(metadata) => {
                        let (tx, rx) = mpsc::channel();
                        self.tx = Some(tx);
                        let _ = self.entry_tx.send(UsartLog {
                            channel: self.channel,
                            kind: UsartLogKind::Stream(LogStream {
                                metadata,
                                values: rx,
                            }),
                        });
                    }
                    Err(_) => return true, // discard this stream
                }
            }
            return false;
        }

        let tx = match &self.tx {
            Some(tx) => tx,
            None => return false,
        };

        let (_, result) = self.decoder.feed(&[byte]);
        match result {
            FeedResult::Done(value) => {
                let _ = tx.send(value);
                false
            }
            FeedResult::EndOfStream => true,
            _ => false,
        }
    }
}

struct StructuredSink {
    channel: u8,
    frame: FrameState,
    streams: HashMap<u64, StreamState>,
    tx: mpsc::Sender<UsartLog>,
}

impl StructuredSink {
    fn new(channel: u8, tx: mpsc::Sender<UsartLog>) -> Self {
        StructuredSink {
            channel,
            frame: FrameState::ReadingId {
                buf: [0; 8],
                pos: 0,
            },
            streams: HashMap::new(),
            tx,
        }
    }

    fn on_byte(&mut self, byte: u8) {
        let frame = std::mem::replace(
            &mut self.frame,
            FrameState::ReadingId {
                buf: [0; 8],
                pos: 0,
            },
        );

        self.frame = match frame {
            FrameState::ReadingId { mut buf, pos } => {
                buf[pos as usize] = byte;
                let pos = pos + 1;
                if pos == 8 {
                    FrameState::ReadingLength {
                        id: u64::from_le_bytes(buf),
                    }
                } else {
                    FrameState::ReadingId { buf, pos }
                }
            }
            FrameState::ReadingLength { id } => {
                if byte == 0 {
                    FrameState::ReadingId {
                        buf: [0; 8],
                        pos: 0,
                    }
                } else {
                    FrameState::ReadingData {
                        id,
                        remaining: byte,
                    }
                }
            }
            FrameState::ReadingData { id, remaining } => {
                let stream = self
                    .streams
                    .entry(id)
                    .or_insert_with(|| StreamState::new(self.channel, self.tx.clone()));

                if stream.feed_byte(byte) {
                    self.streams.remove(&id);
                }

                let remaining = remaining - 1;
                if remaining == 0 {
                    FrameState::ReadingId {
                        buf: [0; 8],
                        pos: 0,
                    }
                } else {
                    FrameState::ReadingData { id, remaining }
                }
            }
        };
    }
}

// --- Display helpers (from emulator CLI) ---

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

fn format_species(
    fmt: &str,
    values: &mpsc::Receiver<OwnedValue>,
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
    }
}

// --- Port helpers ---

fn open_port(name: &str, baud: u32) -> Box<dyn SerialPort> {
    serialport::new(name, baud)
        .timeout(std::time::Duration::MAX)
        .open()
        .unwrap_or_else(|e| panic!("failed to open {name}: {e}"))
}

fn main() {
    let args = Args::parse();

    let log_metadata = args.log_metadata.map(|path| {
        let data = std::fs::read_to_string(&path).expect("failed to read log metadata");
        let entries: Vec<LogMetadataFile> =
            serde_json::from_str(&data).expect("failed to parse log metadata");
        entries
    });

    let usart1 = open_port(&args.usart1, 1_000_000);
    let usart2 = open_port(&args.usart2, 115_200);

    println!(
        "listening on {} and {}",
        usart1.name().unwrap_or_default(),
        usart2.name().unwrap_or_default()
    );

    let (tx, rx) = mpsc::channel::<UsartLog>();

    // USART1: plain text lines
    let tx1 = tx.clone();
    let t1 = thread::spawn(move || {
        let reader = BufReader::new(usart1);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    let _ = tx1.send(UsartLog {
                        channel: 1,
                        kind: UsartLogKind::Line(line),
                    });
                }
                Err(e) => {
                    eprintln!("[usart1] read error: {e}");
                    break;
                }
            }
        }
    });

    // USART2: structured binary log stream
    let tx2 = tx;
    let t2 = thread::spawn(move || {
        let mut sink = StructuredSink::new(2, tx2);
        let mut port = usart2;
        let mut buf = [0u8; 1024];
        loop {
            match port.read(&mut buf) {
                Ok(n) => {
                    for &byte in &buf[..n] {
                        sink.on_byte(byte);
                    }
                }
                Err(e) => {
                    eprintln!("[usart2] read error: {e}");
                    break;
                }
            }
        }
    });

    // Display thread (runs on main)
    let term_width = terminal_width();

    let species_lookup: HashMap<u64, &Species> = log_metadata
        .as_ref()
        .map(|entries| {
            let mut map = HashMap::new();
            for entry in entries {
                for (hash_str, species) in &entry.species {
                    if let Ok(hash) = u64::from_str_radix(hash_str.trim_start_matches("0x"), 16) {
                        map.insert(hash, species);
                    }
                }
            }
            map
        })
        .unwrap_or_default();

    let type_names: HashMap<u64, String> = log_metadata
        .as_ref()
        .map(|entries| {
            let mut map = HashMap::new();
            for entry in entries {
                for (hash_str, value) in &entry.types {
                    let Ok(hash) = u64::from_str_radix(hash_str.trim_start_matches("0x"), 16)
                    else {
                        continue;
                    };
                    if let Some(name) = value.get("name").and_then(|n| n.as_str()) {
                        map.insert(hash, name.to_string());
                    }
                }
            }
            map
        })
        .unwrap_or_default();

    let task_names: Vec<&str> = log_metadata
        .as_ref()
        .and_then(|entries| entries.first())
        .map(|e| e.task_names.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();

    let task_pad = task_names
        .iter()
        .map(|n| n.strip_prefix("sysmodule_").unwrap_or(n).len())
        .max()
        .unwrap_or(0)
        .max(6);
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
            UsartLogKind::Stream(stream) => {
                let source = stream.metadata.source;
                let log_species = stream.metadata.log_species;
                let level = stream.metadata.level;
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
                            "{DIM}species=0x{:x} src={}{RESET}",
                            log_species, source
                        );
                    }
                }
            }
        }
    }

    t1.join().unwrap();
    t2.join().unwrap();
}
