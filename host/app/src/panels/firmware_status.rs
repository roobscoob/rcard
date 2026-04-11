use crate::state::FirmwareHandle;
use crate::theme;
use egui_phosphor::regular as icon;

pub enum FirmwareAction {
    None,
    RunEmulator,
    Flash,
}

pub fn show(ui: &mut egui::Ui, fw: &FirmwareHandle) -> FirmwareAction {
    let mut action = FirmwareAction::None;
    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button(format!("{} Emulate", icon::PLAY)).clicked() {
                    action = FirmwareAction::RunEmulator;
                }
                if ui.button(format!("{} Flash", icon::LIGHTNING)).clicked() {
                    action = FirmwareAction::Flash;
                }
            });
            ui.add_space(8.0);
            build_info_section(ui, fw);
            ui.add_space(12.0);
            place_usage_section(ui, fw);
            ui.add_space(12.0);
            task_info_section(ui, fw);
            ui.add_space(12.0);
            metadata_summary_section(ui, fw);
        });
    action
}

// ── Build Info ──────────────────────────────────────────────────────────

fn build_info_section(ui: &mut egui::Ui, fw: &FirmwareHandle) {
    section_heading(ui, icon::INFO, "Build Info");

    if let Some(build) = &fw.metadata.build {
        info_row(ui, icon::TAG, "Name", &build.name);
        info_row(
            ui,
            icon::HASH,
            "Version",
            build.version.as_deref().unwrap_or("—"),
        );
        info_row(ui, icon::FINGERPRINT, "Build ID", &build.build_id);
        info_row(ui, icon::CLOCK, "Built at", &build.built_at);
        info_row(ui, icon::CPU, "Board", &build.board);
        info_row(ui, icon::LAYOUT, "Layout", &build.layout);
    } else {
        ui.colored_label(theme::TEXT_SECONDARY, "No build metadata (older archive)");
    }

    let file_label = match &fw.path {
        Some(p) => p.display().to_string(),
        None => "(builtin)".to_string(),
    };
    info_row(ui, icon::FILE, "File", &file_label);
}

// ── Place Usage ─────────────────────────────────────────────────────────

fn place_usage_section(ui: &mut egui::Ui, fw: &FirmwareHandle) {
    section_heading(ui, icon::HARD_DRIVE, "Memory Places");

    let Some(config) = &fw.metadata.config else {
        ui.colored_label(theme::TEXT_SECONDARY, "No config data available");
        return;
    };

    for (name, place) in &config.places {
        if place.mappings.is_empty() && place.unmapped {
            continue;
        }

        ui.horizontal(|ui| {
            ui.colored_label(theme::ACCENT, icon::DATABASE);
            ui.strong(name);
            ui.colored_label(theme::TEXT_SECONDARY, format_size(place.size));
            if let Some(offset) = place.offset {
                ui.colored_label(theme::TEXT_DIM, format!("@ {offset:#x}"));
            }
        });

        // Show CPU mappings.
        for mapping in &place.mappings {
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                let flags = format!(
                    "{}{}{}",
                    if mapping.read { "R" } else { "-" },
                    if mapping.write { "W" } else { "-" },
                    if mapping.execute { "X" } else { "-" },
                );
                ui.colored_label(
                    theme::TEXT_DIM,
                    format!("{:#010x}  {}  {}", mapping.address, flags, format_size(mapping.size)),
                );
            });
        }
    }
}

// ── Task Info ───────────────────────────────────────────────────────────

fn task_info_section(ui: &mut egui::Ui, fw: &FirmwareHandle) {
    section_heading(ui, icon::TREE_STRUCTURE, "Tasks");

    let Some(config) = &fw.metadata.config else {
        ui.colored_label(theme::TEXT_SECONDARY, "No config data available");
        return;
    };

    // Kernel.
    ui.horizontal(|ui| {
        ui.colored_label(theme::WARN, icon::SHIELD);
        ui.colored_label(theme::WARN, "kernel");
        ui.colored_label(theme::TEXT_SECONDARY, &config.kernel.crate_info.package.name);
    });

    // Bootloader.
    if let Some(bl) = &config.bootloader {
        ui.horizontal(|ui| {
            ui.colored_label(theme::DEBUG, icon::ROCKET);
            ui.colored_label(theme::DEBUG, "bootloader");
            ui.colored_label(theme::TEXT_SECONDARY, &bl.crate_info.package.name);
        });
    }

    ui.add_space(4.0);

    // Task tree (recursive).
    for task in &config.entries {
        render_task(ui, task, 0);
    }
}

fn render_task(ui: &mut egui::Ui, task: &tfw::config::TaskConfig, depth: usize) {
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * 16.0);

        let (color, task_icon) = if task.supervisor {
            (theme::WARN, icon::SHIELD_CHECK)
        } else {
            (theme::INFO, icon::GEAR)
        };
        ui.colored_label(color, task_icon);
        ui.colored_label(color, &task.crate_info.package.name);

        if let Some(ver) = &task.crate_info.package.version {
            ui.colored_label(theme::TEXT_DIM, format!("v{ver}"));
        }

        ui.colored_label(theme::TEXT_DIM, format!("pri={}", task.priority));

        if task.supervisor {
            ui.colored_label(theme::WARN, "[supervisor]");
        }
    });

    for dep in &task.depends_on {
        render_task(ui, dep, depth + 1);
    }
}

// ── Metadata Summary ────────────────────────────────────────────────────

fn metadata_summary_section(ui: &mut egui::Ui, fw: &FirmwareHandle) {
    section_heading(ui, icon::LIST, "Metadata");

    let meta = &fw.metadata;
    info_row(ui, icon::SCROLL, "Log species", &meta.species.len().to_string());
    info_row(ui, icon::TEXTBOX, "Type names", &meta.type_names.len().to_string());
    info_row(ui, icon::STACK, "Tasks", &meta.task_names.len().to_string());

    if let Some(build) = &meta.build {
        info_row(ui, icon::PACKAGE, "Packages", &build.packages.len().to_string());

        // Collapsible package list.
        ui.collapsing(format!("{} Package versions", icon::CARET_DOWN), |ui| {
            for (name, ver) in &build.packages {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::TEXT_SECONDARY, icon::CUBE);
                    ui.label(name);
                    ui.colored_label(theme::TEXT_DIM, ver);
                });
            }
        });
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn section_heading(ui: &mut egui::Ui, section_icon: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.colored_label(theme::ACCENT, section_icon);
        ui.heading(title);
    });
    ui.separator();
}

fn info_row(ui: &mut egui::Ui, row_icon: &str, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.colored_label(theme::TEXT_DIM, row_icon);
        ui.colored_label(theme::TEXT_SECONDARY, label);
        ui.label(value);
    });
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
