use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::Serialize;
use tfw::build::{
    BuildError, BuildEvent, BuildState, CrateEvent, CrateKind, CrateState, HostCrateEvent,
    HostCrateState, ImageState, MemoryEvent, ResourceUpdate,
};

// ── OutputFormat (clap-facing) ───────────────────────────────────────

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum OutputFormat {
    /// Machine-readable NDJSON event stream (one JSON object per line).
    Json,
    /// Markdown summary digestible by LLMs.
    Md,
}

// ── Shared constants & helpers ───────────────────────────────────────

const CHECK: &str = "✓";
const CROSS: &str = "✗";
const BULLET: &str = "◉";
const CIRCLE: &str = "○";
const ARROW: &str = "→";
const TICKS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "];
const TICK_MS: u64 = 80;
const NAME_COL: usize = 22;

fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn kind_label(kind: CrateKind) -> &'static str {
    match kind {
        CrateKind::Bootloader => "bootloader",
        CrateKind::Kernel => "kernel",
        CrateKind::Task => "task",
    }
}

fn state_label(state: &CrateState) -> &'static str {
    match state {
        CrateState::Building => "building",
        CrateState::Compiled => "compiled",
        CrateState::Measuring => "measuring",
        CrateState::Linking => "linking",
        CrateState::Linked => "linked",
    }
}

fn host_state_label(state: &HostCrateState) -> &'static str {
    match state {
        HostCrateState::Queued => "queued",
        HostCrateState::Building => "building",
        HostCrateState::Running => "running",
        HostCrateState::Done => "done",
    }
}

fn phase_label(state: &BuildState) -> &'static str {
    match state {
        BuildState::Planning => "planning",
        BuildState::CompilingTasks => "compiling_tasks",
        BuildState::Organizing { .. } => "organizing",
        BuildState::CompilingApp => "compiling_app",
        BuildState::ExtractingMetadata => "extracting_metadata",
        BuildState::Packing => "packing",
        BuildState::Done => "done",
    }
}

fn short_name(name: &str) -> &str {
    name.strip_prefix("sysmodule_").unwrap_or(name)
}

// =====================================================================
//  TUI renderer (default)
// =====================================================================

fn kind_style(kind: CrateKind) -> console::Style {
    match kind {
        CrateKind::Bootloader => console::Style::new().blue(),
        CrateKind::Kernel => console::Style::new().yellow(),
        CrateKind::Task => console::Style::new().green(),
    }
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("    {spinner:.blue} {msg}")
        .unwrap()
        .tick_strings(TICKS)
}

fn static_style() -> ProgressStyle {
    ProgressStyle::with_template("    {msg}").unwrap()
}

fn size_suffix(sizes: &[(String, u64)]) -> String {
    if sizes.is_empty() {
        return String::new();
    }
    let total: u64 = sizes.iter().map(|(_, s)| s).sum();
    format!("  {}", style(human_size(total)).bold())
}

const PHASES: &[(&str, fn(&BuildState) -> bool)] = &[
    ("plan", |s| matches!(s, BuildState::Planning)),
    ("compile", |s| matches!(s, BuildState::CompilingTasks)),
    ("organize", |s| matches!(s, BuildState::Organizing { .. })),
    ("app", |s| matches!(s, BuildState::CompilingApp)),
    (
        "metadata",
        |s| matches!(s, BuildState::ExtractingMetadata),
    ),
    ("pack", |s| matches!(s, BuildState::Packing)),
];

struct CrateInfo {
    kind: CrateKind,
    state: CrateState,
    sizes: Vec<(String, u64)>,
    bar: ProgressBar,
}

impl CrateInfo {
    fn total_size(&self) -> u64 {
        self.sizes.iter().map(|(_, s)| s).sum()
    }
}

struct HostInfo {
    bar: ProgressBar,
}

struct TuiRenderer {
    multi: MultiProgress,
    header: ProgressBar,
    pipeline: ProgressBar,
    anchor: ProgressBar,

    app: String,
    board: String,
    phase_idx: Option<usize>,
    organize_n: Option<usize>,

    crates: BTreeMap<String, CrateInfo>,
    hosts: BTreeMap<String, HostInfo>,

    image_size: Option<u64>,
    image_path: Option<PathBuf>,
    start: Instant,
}

impl TuiRenderer {
    fn new(app: &str, board: &str) -> Self {
        let m = MultiProgress::new();

        let header = m.add(ProgressBar::new_spinner());
        header.set_style(
            ProgressStyle::with_template("  {spinner:.cyan.bold} {wide_msg} {elapsed:.dim}")
                .unwrap()
                .tick_strings(TICKS),
        );
        header.set_message(format!(
            "{} · {}",
            style(app).bold(),
            style(board).dim()
        ));
        header.enable_steady_tick(Duration::from_millis(TICK_MS));

        let pipeline = m.add(ProgressBar::new_spinner());
        pipeline.set_style(ProgressStyle::with_template("  {msg}").unwrap());

        let spacer = m.add(ProgressBar::new_spinner());
        spacer.set_style(ProgressStyle::with_template("").unwrap());
        spacer.finish();

        let anchor = m.add(ProgressBar::new_spinner());
        anchor.set_style(ProgressStyle::with_template("").unwrap());

        let r = Self {
            multi: m,
            header,
            pipeline,
            anchor,
            app: app.into(),
            board: board.into(),
            phase_idx: None,
            organize_n: None,
            crates: BTreeMap::new(),
            hosts: BTreeMap::new(),
            image_size: None,
            image_path: None,
            start: Instant::now(),
        };
        r.draw_pipeline();
        r
    }

    fn draw_pipeline(&self) {
        let sep = format!("  {}  ", style("·").dim());
        let parts: Vec<String> = PHASES
            .iter()
            .enumerate()
            .map(|(i, (label, _))| {
                let mut text = (*label).to_string();
                if *label == "organize" {
                    if let Some(n) = self.organize_n {
                        text = format!("{label} ({n})");
                    }
                }
                match self.phase_idx {
                    Some(idx) if i < idx => {
                        format!("{} {}", style(CHECK).green(), style(&text).green())
                    }
                    Some(idx) if i == idx => {
                        format!(
                            "{} {}",
                            style(BULLET).cyan().bold(),
                            style(&text).cyan().bold()
                        )
                    }
                    _ => format!("{} {}", style(CIRCLE).dim(), style(&text).dim()),
                }
            })
            .collect();
        self.pipeline.set_message(parts.join(&sep));
    }

    fn ensure_crate(&mut self, name: &str, kind: CrateKind) {
        if self.crates.contains_key(name) {
            return;
        }
        let bar = self
            .multi
            .insert_before(&self.anchor, ProgressBar::new_spinner());
        bar.set_style(spinner_style());
        bar.enable_steady_tick(Duration::from_millis(TICK_MS));
        self.crates.insert(
            name.to_string(),
            CrateInfo {
                kind,
                state: CrateState::Building,
                sizes: Vec::new(),
                bar,
            },
        );
    }

    fn draw_crate_active(&self, name: &str) {
        let Some(info) = self.crates.get(name) else {
            return;
        };
        let ks = kind_style(info.kind);
        let dn = format!("{:<w$}", short_name(name), w = NAME_COL);
        let label = match info.state {
            CrateState::Building => "compiling",
            CrateState::Measuring => "measuring",
            CrateState::Linking => "linking",
            _ => "",
        };
        let sz = size_suffix(&info.sizes);
        info.bar
            .set_message(format!("{}  {label}{sz}", ks.apply_to(&dn)));
    }

    fn draw_crate_paused(&self, name: &str) {
        let Some(info) = self.crates.get(name) else {
            return;
        };
        let ks = kind_style(info.kind);
        let dn = format!("{:<w$}", short_name(name), w = NAME_COL);
        let sz = size_suffix(&info.sizes);
        info.bar.disable_steady_tick();
        info.bar.set_style(static_style());
        info.bar.set_message(format!(
            "{} {}  {}{sz}",
            style(CHECK).green(),
            ks.apply_to(&dn),
            style("compiled").dim(),
        ));
    }

    fn draw_crate_done(&self, name: &str) {
        let Some(info) = self.crates.get(name) else {
            return;
        };
        let ks = kind_style(info.kind);
        let dn = format!("{:<w$}", short_name(name), w = NAME_COL);
        let sz = size_suffix(&info.sizes);
        info.bar.set_style(static_style());
        info.bar.finish_with_message(format!(
            "{} {}{sz}",
            style(CHECK).green(),
            ks.apply_to(&dn),
        ));
    }

    fn ensure_host(&mut self, name: &str) {
        if self.hosts.contains_key(name) {
            return;
        }
        let bar = self
            .multi
            .insert_before(&self.anchor, ProgressBar::new_spinner());
        bar.set_style(spinner_style());
        bar.enable_steady_tick(Duration::from_millis(TICK_MS));
        self.hosts.insert(name.to_string(), HostInfo { bar });
    }

    fn handle(&mut self, event: BuildEvent) {
        match event {
            BuildEvent::Build(ref state) => {
                if let BuildState::Organizing { regions_placed } = state {
                    self.organize_n = Some(*regions_placed);
                }
                if matches!(state, BuildState::Done) {
                    self.phase_idx = Some(PHASES.len());
                } else {
                    self.phase_idx = PHASES.iter().position(|(_, f)| f(state));
                }
                self.draw_pipeline();
            }

            BuildEvent::Crate {
                name,
                kind,
                update: ResourceUpdate::State(state),
            } => {
                self.ensure_crate(&name, kind);
                self.crates.get_mut(&name).unwrap().state = state.clone();
                match state {
                    CrateState::Building | CrateState::Measuring | CrateState::Linking => {
                        self.crates[&name].bar.set_style(spinner_style());
                        self.crates[&name]
                            .bar
                            .enable_steady_tick(Duration::from_millis(TICK_MS));
                        self.draw_crate_active(&name);
                    }
                    CrateState::Compiled => self.draw_crate_paused(&name),
                    CrateState::Linked => self.draw_crate_done(&name),
                }
            }

            BuildEvent::Crate {
                name,
                kind,
                update: ResourceUpdate::Event(event),
            } => {
                self.ensure_crate(&name, kind);
                match event {
                    CrateEvent::Sized { region, size } => {
                        self.crates.get_mut(&name).unwrap().sizes.push((region, size));
                        self.draw_crate_active(&name);
                    }
                    CrateEvent::CargoMessage(msg) => {
                        if let Ok(decoded) = msg.decode() {
                            if let escargot::format::Message::CompilerMessage(cm) = decoded {
                                if let Some(rendered) = cm.message.rendered {
                                    let _ = self.multi.println(rendered);
                                }
                            }
                        }
                    }
                    CrateEvent::CargoError(e) => {
                        let _ = self
                            .multi
                            .println(format!("{}", style(format!("cargo: {e}")).red()));
                    }
                }
            }

            BuildEvent::HostCrate {
                name,
                update: ResourceUpdate::State(state),
            } => {
                self.ensure_host(&name);
                let info = &self.hosts[&name];
                let dn = format!("{:<w$}", &name, w = NAME_COL);
                match state {
                    HostCrateState::Queued => {
                        info.bar.set_message(format!(
                            "{}  {}",
                            style(&dn).cyan(),
                            style("queued").dim(),
                        ));
                    }
                    HostCrateState::Building => {
                        info.bar
                            .set_message(format!("{}  compiling", style(&dn).cyan()));
                    }
                    HostCrateState::Running => {
                        info.bar
                            .set_message(format!("{}  running", style(&dn).cyan()));
                    }
                    HostCrateState::Done => {
                        info.bar.set_style(static_style());
                        info.bar.finish_with_message(format!(
                            "{} {}",
                            style(CHECK).green(),
                            style(&dn).cyan(),
                        ));
                    }
                }
            }

            BuildEvent::HostCrate {
                update: ResourceUpdate::Event(event),
                ..
            } => match event {
                HostCrateEvent::CargoMessage(msg) => {
                    if let Ok(decoded) = msg.decode() {
                        if let escargot::format::Message::CompilerMessage(cm) = decoded {
                            if let Some(rendered) = cm.message.rendered {
                                let _ = self.multi.println(rendered);
                            }
                        }
                    }
                }
                HostCrateEvent::CargoError(e) => {
                    let _ = self
                        .multi
                        .println(format!("{}", style(format!("cargo: {e}")).red()));
                }
            },

            BuildEvent::Image(ResourceUpdate::State(ImageState::Assembled { size })) => {
                self.image_size = Some(size);
            }
            BuildEvent::Image(ResourceUpdate::State(ImageState::Archived { path })) => {
                self.image_path = Some(path);
            }

            _ => {}
        }
    }

    fn clear_bars(&self) {
        self.header.finish_and_clear();
        self.pipeline.finish_and_clear();
        self.anchor.finish_and_clear();
        for info in self.crates.values() {
            info.bar.finish_and_clear();
        }
        for info in self.hosts.values() {
            info.bar.finish_and_clear();
        }
    }

    fn finish_ok(&mut self) {
        self.clear_bars();
        let elapsed = self.start.elapsed().as_secs_f64();

        eprintln!();
        eprintln!(
            "  {} {} · {} · {:.1}s",
            style(CHECK).green().bold(),
            style(&self.app).bold(),
            style(&self.board).dim(),
            elapsed,
        );

        if self.crates.values().any(|c| c.total_size() > 0) {
            eprintln!();
            let name_w = self
                .crates
                .keys()
                .map(|n| short_name(n).len())
                .max()
                .unwrap_or(8);
            for (name, info) in &self.crates {
                let total = info.total_size();
                if total == 0 {
                    continue;
                }
                let dn = short_name(name);
                eprintln!(
                    "    {} {:>10}",
                    kind_style(info.kind).apply_to(format!("{:<w$}", dn, w = name_w)),
                    human_size(total),
                );
            }
        }

        if let Some(path) = &self.image_path {
            eprintln!();
            let fname = path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            let sz = self.image_size.map(human_size).unwrap_or_default();
            eprintln!(
                "  {} {} · {}",
                style(ARROW).dim(),
                style(&fname).bold(),
                style(&sz).dim(),
            );
        }
        eprintln!();
    }

    fn finish_err(&mut self, error: &BuildError) {
        self.clear_bars();
        let elapsed = self.start.elapsed().as_secs_f64();
        eprintln!();
        eprintln!(
            "  {} {} · {} · {:.1}s",
            style(CROSS).red().bold(),
            style(&self.app).bold(),
            style(&self.board).dim(),
            elapsed,
        );
        eprintln!();
        let msg = format!("{error}");
        let mut lines = msg.lines();
        if let Some(first) = lines.next() {
            eprintln!("    {}", style(first).red().bold());
        }
        for line in lines {
            eprintln!("    {}", style(line).red());
        }
        eprintln!();
    }
}

// =====================================================================
//  JSON renderer (--output-format json)
// =====================================================================

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum JsonEvent {
    Phase {
        name: String,
    },
    CrateState {
        name: String,
        kind: String,
        state: String,
    },
    CrateSized {
        name: String,
        region: String,
        size: u64,
    },
    CargoMessage {
        #[serde(rename = "crate")]
        krate: String,
        message: String,
    },
    CargoError {
        #[serde(rename = "crate")]
        krate: String,
        error: String,
    },
    HostCrateState {
        name: String,
        state: String,
    },
    MemoryAllocated {
        place: String,
        owner: String,
        region: String,
        base: u64,
        size: u64,
    },
    ImageAssembled {
        size: u64,
    },
    ImageArchived {
        path: String,
    },
    BuildComplete {
        success: bool,
        elapsed_secs: f64,
    },
    BuildError {
        error: String,
    },
}

struct JsonOutput {
    start: Instant,
}

impl JsonOutput {
    fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    fn emit(&self, event: JsonEvent) {
        if let Ok(json) = serde_json::to_string(&event) {
            println!("{json}");
        }
    }

    fn handle(&mut self, event: BuildEvent) {
        match event {
            BuildEvent::Build(ref state) => {
                self.emit(JsonEvent::Phase {
                    name: phase_label(state).into(),
                });
            }

            BuildEvent::Crate {
                name,
                kind,
                update: ResourceUpdate::State(state),
            } => {
                self.emit(JsonEvent::CrateState {
                    name,
                    kind: kind_label(kind).into(),
                    state: state_label(&state).into(),
                });
            }

            BuildEvent::Crate {
                name,
                update: ResourceUpdate::Event(event),
                ..
            } => match event {
                CrateEvent::Sized { region, size } => {
                    self.emit(JsonEvent::CrateSized { name, region, size });
                }
                CrateEvent::CargoMessage(msg) => {
                    if let Ok(decoded) = msg.decode() {
                        if let escargot::format::Message::CompilerMessage(cm) = decoded {
                            if let Some(rendered) = cm.message.rendered {
                                self.emit(JsonEvent::CargoMessage {
                                    krate: name,
                                    message: rendered.to_string(),
                                });
                            }
                        }
                    }
                }
                CrateEvent::CargoError(e) => {
                    self.emit(JsonEvent::CargoError {
                        krate: name,
                        error: format!("{e}"),
                    });
                }
            },

            BuildEvent::HostCrate {
                name,
                update: ResourceUpdate::State(state),
            } => {
                self.emit(JsonEvent::HostCrateState {
                    name,
                    state: host_state_label(&state).into(),
                });
            }

            BuildEvent::HostCrate {
                name,
                update: ResourceUpdate::Event(event),
            } => match event {
                HostCrateEvent::CargoMessage(msg) => {
                    if let Ok(decoded) = msg.decode() {
                        if let escargot::format::Message::CompilerMessage(cm) = decoded {
                            if let Some(rendered) = cm.message.rendered {
                                self.emit(JsonEvent::CargoMessage {
                                    krate: name,
                                    message: rendered.to_string(),
                                });
                            }
                        }
                    }
                }
                HostCrateEvent::CargoError(e) => {
                    self.emit(JsonEvent::CargoError {
                        krate: name,
                        error: format!("{e}"),
                    });
                }
            },

            BuildEvent::Memory {
                place,
                update:
                    ResourceUpdate::Event(MemoryEvent::Allocated {
                        owner,
                        region,
                        base,
                        size,
                        ..
                    }),
            } => {
                self.emit(JsonEvent::MemoryAllocated {
                    place,
                    owner,
                    region,
                    base,
                    size,
                });
            }

            BuildEvent::Image(ResourceUpdate::State(ImageState::Assembled { size })) => {
                self.emit(JsonEvent::ImageAssembled { size });
            }
            BuildEvent::Image(ResourceUpdate::State(ImageState::Archived { path })) => {
                self.emit(JsonEvent::ImageArchived {
                    path: path.display().to_string(),
                });
            }

            _ => {}
        }
    }

    fn finish_ok(&self) {
        self.emit(JsonEvent::BuildComplete {
            success: true,
            elapsed_secs: self.start.elapsed().as_secs_f64(),
        });
    }

    fn finish_err(&self, error: &BuildError) {
        self.emit(JsonEvent::BuildError {
            error: format!("{error}"),
        });
        self.emit(JsonEvent::BuildComplete {
            success: false,
            elapsed_secs: self.start.elapsed().as_secs_f64(),
        });
    }
}

// =====================================================================
//  Markdown renderer (--output-format md)
// =====================================================================

struct MdCrate {
    kind: CrateKind,
    sizes: Vec<(String, u64)>,
}

struct MdAlloc {
    place: String,
    owner: String,
    region: String,
    base: u64,
    size: u64,
}

struct MdOutput {
    app: String,
    board: String,
    start: Instant,
    crates: BTreeMap<String, MdCrate>,
    host_crates: BTreeMap<String, String>,
    allocations: Vec<MdAlloc>,
    diagnostics: Vec<String>,
    image_size: Option<u64>,
    image_path: Option<PathBuf>,
}

impl MdOutput {
    fn new(app: &str, board: &str) -> Self {
        Self {
            app: app.into(),
            board: board.into(),
            start: Instant::now(),
            crates: BTreeMap::new(),
            host_crates: BTreeMap::new(),
            allocations: Vec::new(),
            diagnostics: Vec::new(),
            image_size: None,
            image_path: None,
        }
    }

    fn handle(&mut self, event: BuildEvent) {
        match event {
            BuildEvent::Crate {
                name,
                kind,
                update: ResourceUpdate::State(_),
            } => {
                self.crates
                    .entry(name)
                    .or_insert_with(|| MdCrate {
                        kind,
                        sizes: Vec::new(),
                    });
            }

            BuildEvent::Crate {
                name,
                kind,
                update: ResourceUpdate::Event(event),
            } => {
                let entry = self
                    .crates
                    .entry(name.clone())
                    .or_insert_with(|| MdCrate {
                        kind,
                        sizes: Vec::new(),
                    });
                match event {
                    CrateEvent::Sized { region, size } => {
                        entry.sizes.push((region, size));
                    }
                    CrateEvent::CargoMessage(msg) => {
                        if let Ok(decoded) = msg.decode() {
                            if let escargot::format::Message::CompilerMessage(cm) = decoded {
                                if let Some(rendered) = cm.message.rendered {
                                    self.diagnostics.push(rendered.to_string());
                                }
                            }
                        }
                    }
                    CrateEvent::CargoError(e) => {
                        self.diagnostics
                            .push(format!("[{name}] cargo error: {e}"));
                    }
                }
            }

            BuildEvent::HostCrate {
                name,
                update: ResourceUpdate::State(state),
            } => {
                self.host_crates
                    .insert(name, host_state_label(&state).into());
            }

            BuildEvent::HostCrate {
                name,
                update: ResourceUpdate::Event(event),
            } => match event {
                HostCrateEvent::CargoMessage(msg) => {
                    if let Ok(decoded) = msg.decode() {
                        if let escargot::format::Message::CompilerMessage(cm) = decoded {
                            if let Some(rendered) = cm.message.rendered {
                                self.diagnostics.push(rendered.to_string());
                            }
                        }
                    }
                }
                HostCrateEvent::CargoError(e) => {
                    self.diagnostics
                        .push(format!("[{name}] cargo error: {e}"));
                }
            },

            BuildEvent::Memory {
                place,
                update:
                    ResourceUpdate::Event(MemoryEvent::Allocated {
                        owner,
                        region,
                        base,
                        size,
                        ..
                    }),
            } => {
                self.allocations.push(MdAlloc {
                    place,
                    owner,
                    region,
                    base,
                    size,
                });
            }

            BuildEvent::Image(ResourceUpdate::State(ImageState::Assembled { size })) => {
                self.image_size = Some(size);
            }
            BuildEvent::Image(ResourceUpdate::State(ImageState::Archived { path })) => {
                self.image_path = Some(path);
            }

            _ => {}
        }
    }

    fn render(&self, success: bool, error_msg: Option<String>) {
        let elapsed = self.start.elapsed().as_secs_f64();
        let status = if success { "Success" } else { "Failed" };

        println!("# Build Report: {} · {}", self.app, self.board);
        println!();
        println!("This is the output of `rcard-cli build`. The build system \
                  compiles Nickel config into firmware crates (Rust, cross-compiled \
                  to thumbv8m), links them into a memory layout, and packs the \
                  result into a `.tfw` archive.");
        println!();
        println!("- **App config:** `{}`", self.app);
        println!("- **Board:** `{}`", self.board);
        println!("- **Status:** {status}");
        println!("- **Duration:** {elapsed:.1}s");
        if let Some(path) = &self.image_path {
            let fname = path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            let sz = self.image_size.map(human_size).unwrap_or_default();
            println!("- **Output:** `{fname}` ({sz})");
        }

        // Build pipeline explanation + error context up front
        if let Some(err) = &error_msg {
            println!();
            println!("## Error");
            println!();
            println!("The build pipeline is: plan → compile tasks → organize \
                      (memory layout) → compile app → extract metadata → pack.");
            println!();
            println!("The build failed with:");
            println!();
            println!("```");
            println!("{err}");
            println!("```");
            println!();
            println!("To investigate, look at the crate source under \
                      `firmware/`. Kernel and bootloader live in `firmware/hubris/`. \
                      Task crates are in `firmware/task/` or `firmware/sysmodule/`. \
                      Board configs are Nickel files under `firmware/boards/`.");
        }

        // Crates table
        let has_sizes = self.crates.values().any(|c| !c.sizes.is_empty());
        if !self.crates.is_empty() {
            println!();
            println!("## Crates");
            println!();
            println!("Each crate is one of: **bootloader** (runs first, hands off to kernel), \
                      **kernel** (scheduler + syscalls), or **task** (userspace, \
                      including sysmodules which are privileged tasks).");
            if !success {
                println!();
                println!("> **Note:** Build did not complete — this list may be incomplete. \
                          Only crates whose compilation was attempted are shown.");
            }
            println!();
            if has_sizes {
                println!("| Name | Kind | Total | Regions |");
                println!("|------|------|------:|---------|");
            } else {
                println!("| Name | Kind |");
                println!("|------|------|");
            }
            for (name, info) in &self.crates {
                let dn = short_name(name);
                let kind = kind_label(info.kind);
                if has_sizes {
                    let total: u64 = info.sizes.iter().map(|(_, s)| s).sum();
                    let regions: Vec<String> = info
                        .sizes
                        .iter()
                        .map(|(r, s)| format!("{r} {}", human_size(*s)))
                        .collect();
                    let regions_str = if regions.is_empty() {
                        "—".into()
                    } else {
                        regions.join(", ")
                    };
                    let total_str = if total > 0 {
                        human_size(total)
                    } else {
                        "—".into()
                    };
                    println!("| {dn} | {kind} | {total_str} | {regions_str} |");
                } else {
                    println!("| {dn} | {kind} |");
                }
            }
        }

        // Host crates
        if !self.host_crates.is_empty() {
            println!();
            println!("## Host Crates");
            println!();
            println!("Host crates run on the build machine (not the target). \
                      They generate code or data consumed by firmware crates.");
            println!();
            for (name, state) in &self.host_crates {
                println!("- **{name}**: {state}");
            }
        }

        // Memory allocations
        if !self.allocations.is_empty() {
            println!();
            println!("## Memory Map");
            println!();
            println!("Each crate's sections (code, data, stack, etc.) are placed \
                      into physical memory devices. Address collisions or \
                      overflows here indicate a layout problem in the Nickel config.");
            println!();
            println!("| Owner | Region | Device | Address | Size |");
            println!("|-------|--------|--------|---------|-----:|");
            for a in &self.allocations {
                println!(
                    "| {} | {} | {} | {:#010x} | {} |",
                    short_name(&a.owner),
                    a.region,
                    a.place,
                    a.base,
                    human_size(a.size),
                );
            }
        }

        // Diagnostics
        if !self.diagnostics.is_empty() {
            println!();
            println!("## Diagnostics");
            println!();
            println!("Compiler warnings and errors emitted during the build. \
                      Entries prefixed with `[crate_name]` identify the source crate.");
            println!();
            for d in &self.diagnostics {
                println!("```");
                println!("{d}");
                println!("```");
            }
        }
    }

    fn finish_ok(&self) {
        self.render(true, None);
    }

    fn finish_err(&self, error: &BuildError) {
        self.render(false, Some(format!("{error}")));
    }
}

// =====================================================================
//  Output dispatcher
// =====================================================================

enum Output {
    Tui(TuiRenderer),
    Json(JsonOutput),
    Md(MdOutput),
}

impl Output {
    fn handle(&mut self, event: BuildEvent) {
        match self {
            Self::Tui(r) => r.handle(event),
            Self::Json(j) => j.handle(event),
            Self::Md(m) => m.handle(event),
        }
    }

    fn finish_ok(&mut self) {
        match self {
            Self::Tui(r) => r.finish_ok(),
            Self::Json(j) => j.finish_ok(),
            Self::Md(m) => m.finish_ok(),
        }
    }

    fn finish_err(&mut self, error: &BuildError) {
        match self {
            Self::Tui(r) => r.finish_err(error),
            Self::Json(j) => j.finish_err(error),
            Self::Md(m) => m.finish_err(error),
        }
    }
}

// =====================================================================
//  Entry point
// =====================================================================

pub async fn run(
    firmware_dir: PathBuf,
    config: String,
    board: String,
    layout: String,
    out: PathBuf,
    format: Option<OutputFormat>,
) {
    let app = config
        .strip_suffix(".ncl")
        .unwrap_or(&config)
        .to_string();
    let board_short = board
        .rsplit('/')
        .next()
        .and_then(|f| f.strip_suffix(".ncl"))
        .unwrap_or(&board)
        .to_string();

    let mut output = match format {
        None => Output::Tui(TuiRenderer::new(&app, &board_short)),
        Some(OutputFormat::Json) => Output::Json(JsonOutput::new()),
        Some(OutputFormat::Md) => Output::Md(MdOutput::new(&app, &board_short)),
    };

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

    while let Some(event) = rx.recv().await {
        output.handle(event);
    }

    match handle.await.unwrap() {
        Ok(_) => output.finish_ok(),
        Err(e) => {
            output.finish_err(&e);
            std::process::exit(1);
        }
    }
}
