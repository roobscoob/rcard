//! Unified build / firmware panel.
//!
//! Renders a single page that morphs across the build lifecycle:
//! - `Running`: hero shows current phase + stepper + live crate states
//! - `Succeeded`: hero shows BUILT + compact build trace + frozen metadata
//! - `Failed`: hero turns red + failed crate expanded with its error
//!
//! The same component handles every state by reading from the structured
//! `BuildHandle` fields — phase, crates, allocations, image, diagnostics.

use std::time::Duration;

use egui_phosphor::regular as icon;
use egui_taffy::taffy::prelude::*;
use egui_taffy::{tui as taffy_tui, TuiBuilderLogic};

use crate::state::{
    BuildHandle, BuildStatus, CrateBuildState, CrateKind, CrateProgress, Diagnostic,
    DiagnosticLevel, ImageProgress, MemoryAllocation, PipelinePhase,
};
use crate::theme;

// All 7 pipeline phases, in stable order, for rendering the stepper.
const PHASE_ORDER: &[PipelinePhase] = &[
    PipelinePhase::Planning,
    PipelinePhase::CompilingTasks,
    PipelinePhase::Organizing { regions_placed: 0 },
    PipelinePhase::CompilingApp,
    PipelinePhase::ExtractingMetadata,
    PipelinePhase::Packing,
    PipelinePhase::Done,
];

fn phase_icon(phase: &PipelinePhase) -> &'static str {
    match phase {
        PipelinePhase::Planning => icon::MAGIC_WAND,
        PipelinePhase::CompilingTasks => icon::GEAR_SIX,
        PipelinePhase::Organizing { .. } => icon::RULER,
        PipelinePhase::CompilingApp => icon::SHIELD,
        PipelinePhase::ExtractingMetadata => icon::FLASK,
        PipelinePhase::Packing => icon::PACKAGE,
        PipelinePhase::Done => icon::CHECK,
    }
}

/// User action triggered by a button in the panel. Empty in most frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelAction {
    None,
    /// Flash this firmware to a device — `FirmwareId` resolved from the
    /// build's Succeeded state or from the firmware snapshot.
    Flash(crate::state::FirmwareId),
    /// Launch the emulator against this firmware.
    RunEmulator(crate::state::FirmwareId),
    /// Delete the build record and close its tile. Only offered for
    /// live builds that have finished (Succeeded / Failed) — doesn't
    /// delete the firmware artifact, just the in-app build state.
    DeleteBuild(crate::state::BuildId),
}

pub fn show(ui: &mut egui::Ui, build: &BuildHandle) -> PanelAction {
    // Keep the UI ticking while a build is running so timers & spinners
    // update smoothly.
    if matches!(build.status, BuildStatus::Running) {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }

    let mut action = PanelAction::None;
    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            // Bound all content to the visible width so cards can't
            // overflow the pane on the right.
            ui.set_max_width(ui.available_width());
            ui.add_space(4.0);
            action = hero(ui, build);
            ui.add_space(12.0);
            stats_strip(ui, build);
            ui.add_space(12.0);
            pipeline_section(ui, build);
            ui.add_space(12.0);
            two_column_body(ui, build);
            ui.add_space(12.0);
            diagnostics_section(ui, build);
            ui.add_space(10.0);
            pipeline_log(ui, build);
            ui.add_space(8.0);
            footer(ui, build);
            ui.add_space(4.0);
        });
    action
}

fn two_column_body(ui: &mut egui::Ui, build: &BuildHandle) {
    let right_w: f32 = 380.0;
    let gap = 12.0;
    // On narrow viewports collapse to a single column.
    if ui.available_width() < right_w * 2.5 {
        crates_card(ui, build);
        ui.add_space(gap);
        memory_card(ui, build);
        ui.add_space(gap);
        resources_card(ui, build);
        return;
    }
    taffy_tui(ui, egui::Id::new(("body", build.id.0)))
        .reserve_available_width()
        .style(Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Row,
            align_items: Some(AlignItems::Start),
            gap: Size {
                width: length(gap),
                height: length(0.0),
            },
            size: Size {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        })
        .show(|tui| {
            // Left column — crates card grows to fill remaining width.
            tui.style(Style {
                flex_grow: 1.0,
                flex_basis: length(0.0),
                min_size: Size {
                    width: length(0.0),
                    height: auto(),
                },
                ..Default::default()
            })
            .ui(|ui| {
                crates_card(ui, build);
            });
            // Right column — fixed 380px, memory + resources stacked.
            // Image/archive info now lives in the bottom footer rather
            // than its own card.
            tui.style(Style {
                flex_shrink: 0.0,
                size: Size {
                    width: length(right_w),
                    height: auto(),
                },
                ..Default::default()
            })
            .ui(|ui| {
                memory_card(ui, build);
                ui.add_space(12.0);
                resources_card(ui, build);
            });
        });
}

// ── Resources card ──────────────────────────────────────────────────────

fn resources_card(ui: &mut egui::Ui, build: &BuildHandle) {
    if build.resources.is_empty() {
        return;
    }
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(theme::ACCENT, icon::PLUGS_CONNECTED);
            ui.label(
                egui::RichText::new("RESOURCES")
                    .monospace()
                    .strong()
                    .size(13.0)
                    .color(theme::TEXT_PRIMARY),
            );
            let total_methods: usize = build.resources.iter().map(|r| r.methods.len()).sum();
            ui.colored_label(
                theme::TEXT_DIM,
                format!(
                    "{} resources · {} methods",
                    build.resources.len(),
                    total_methods
                ),
            );
        });
        ui.add_space(6.0);

        for res in &build.resources {
            let id = ui.make_persistent_id(("resource_row", build.id.0, res.name.as_str()));
            let collapsing = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                false,
            );

            ui.add_space(3.0);
            egui::Frame::NONE
                .fill(theme::BG)
                .corner_radius(egui::CornerRadius::same(5))
                .inner_margin(egui::Margin {
                    left: 10,
                    right: 10,
                    top: 4,
                    bottom: 4,
                })
                .show(ui, |ui| {
                    collapsing
                        .show_header(ui, |ui| {
                            ui.colored_label(theme::ACCENT, icon::PLUGS);
                            ui.label(
                                egui::RichText::new(&res.name)
                                    .monospace()
                                    .strong()
                                    .size(12.0)
                                    .color(theme::TEXT_PRIMARY),
                            );
                            if !res.provider_task.is_empty() {
                                ui.colored_label(
                                    theme::TEXT_DIM,
                                    egui::RichText::new(&res.provider_task)
                                        .monospace()
                                        .size(11.0),
                                );
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} methods",
                                            res.methods.len()
                                        ))
                                        .monospace()
                                        .size(10.0)
                                        .color(theme::TEXT_SECONDARY),
                                    );
                                },
                            );
                        })
                        .body_unindented(|ui| {
                            // Same pattern as the crate body — taffy
                            // owns padding + fill, no egui::Frame.
                            taffy_tui(
                                ui,
                                egui::Id::new((
                                    "resource_row_body",
                                    build.id.0,
                                    res.name.as_str(),
                                )),
                            )
                            .reserve_available_width()
                            .style(Style {
                                display: Display::Flex,
                                flex_direction: FlexDirection::Column,
                                size: Size {
                                    width: percent(1.0),
                                    height: auto(),
                                },
                                ..Default::default()
                            })
                            .show(|tui| {
                                tui.style(Style {
                                    display: Display::Flex,
                                    flex_direction: FlexDirection::Column,
                                    size: Size {
                                        width: percent(1.0),
                                        height: auto(),
                                    },
                                    padding: Rect {
                                        left: length(24.0),
                                        right: length(10.0),
                                        top: length(6.0),
                                        bottom: length(6.0),
                                    },
                                    ..Default::default()
                                })
                                .add_with_background_ui(
                                    |ui, container| {
                                        ui.painter().rect_filled(
                                            container.full_container(),
                                            egui::CornerRadius::ZERO,
                                            theme::PANEL,
                                        );
                                    },
                                    |tui, _| {
                                        tui.style(Style {
                                            size: Size {
                                                width: percent(1.0),
                                                height: auto(),
                                            },
                                            ..Default::default()
                                        })
                                        .ui(|ui| {
                                            for m in &res.methods {
                                                ui.label(
                                                    egui::RichText::new(m)
                                                        .monospace()
                                                        .size(11.0)
                                                        .color(
                                                            theme::TEXT_SECONDARY,
                                                        ),
                                                );
                                            }
                                        });
                                    },
                                );
                            });
                        });
                });
        }
    });
}

// ── Hero ────────────────────────────────────────────────────────────────

fn hero(ui: &mut egui::Ui, build: &BuildHandle) -> PanelAction {
    let mut action = PanelAction::None;
    card(ui, |ui| {
        ui.horizontal(|ui| {
            // Left: status pill + title + subtitle
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    status_pill(ui, build);
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(&build.config.config)
                            .size(22.0)
                            .strong()
                            .color(theme::TEXT_PRIMARY),
                    );
                });
                ui.horizontal(|ui| {
                    ui.colored_label(theme::TEXT_DIM, icon::FILE_CODE);
                    ui.colored_label(
                        theme::TEXT_SECONDARY,
                        format!("apps/{}", build.config.config),
                    );
                    ui.colored_label(theme::TEXT_DIM, "·");
                    ui.colored_label(theme::TEXT_DIM, icon::CPU);
                    ui.colored_label(
                        theme::TEXT_SECONDARY,
                        format!("boards/{}", build.config.board),
                    );
                    ui.colored_label(theme::TEXT_DIM, "·");
                    ui.colored_label(theme::TEXT_DIM, icon::LAYOUT);
                    ui.colored_label(
                        theme::TEXT_SECONDARY,
                        format!("layouts/{}", build.config.layout),
                    );
                });
                if let Some(uuid) = &build.uuid {
                    ui.horizontal(|ui| {
                        ui.colored_label(theme::TEXT_DIM, icon::FINGERPRINT);
                        ui.label(
                            egui::RichText::new(uuid)
                                .monospace()
                                .size(11.0)
                                .color(theme::TEXT_DIM),
                        )
                        .on_hover_text("Build UUID — persists into the .tfw archive");
                    });
                }
            });
            // Push right-side content to far right.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Delete button — shown for any BuildHandle that
                // isn't currently running. `remove_build` drops the
                // handle from `state.builds` and closes the tile;
                // the firmware artifact on disk (if any) stays.
                if !matches!(build.status, BuildStatus::Running) {
                    let del = ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(icon::TRASH)
                                    .size(14.0)
                                    .color(theme::ERROR),
                            )
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(egui::Stroke::new(
                                1.0,
                                theme::ERROR.gamma_multiply(0.4),
                            ))
                            .min_size(egui::vec2(28.0, 28.0)),
                        )
                        .on_hover_text("Delete this build record");
                    if del.clicked() {
                        action = PanelAction::DeleteBuild(build.id);
                    }
                    ui.add_space(4.0);
                }
                // CTA buttons take precedence on the right edge when we
                // have a firmware to act on.
                if let BuildStatus::Succeeded {
                    firmware_id: Some(fw_id),
                    ..
                } = &build.status
                {
                    // Primary CTA: Flash — accent-filled, rounded,
                    // generous padding, icon + label. Matches the
                    // mockup's solid blue pill button.
                    let flash = ui.add(
                        egui::Button::new(
                            egui::RichText::new(format!("{}  Flash", icon::LIGHTNING))
                                .strong()
                                .size(13.0)
                                .color(theme::BG),
                        )
                        .fill(theme::ACCENT)
                        .corner_radius(egui::CornerRadius::same(6))
                        .min_size(egui::vec2(110.0, 32.0)),
                    );
                    if flash.clicked() {
                        action = PanelAction::Flash(*fw_id);
                    }
                    ui.add_space(6.0);
                    // Secondary: Emulate — ghost style, thin border,
                    // transparent fill, same corner radius.
                    let emu = ui.add(
                        egui::Button::new(
                            egui::RichText::new(format!("{}  Emulate", icon::PLAY))
                                .size(13.0)
                                .color(theme::TEXT_PRIMARY),
                        )
                        .fill(egui::Color32::TRANSPARENT)
                        .stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
                        .corner_radius(egui::CornerRadius::same(6))
                        .min_size(egui::vec2(110.0, 32.0)),
                    );
                    if emu.clicked() {
                        action = PanelAction::RunEmulator(*fw_id);
                    }
                    ui.add_space(6.0);
                }
                // Show the elapsed block whenever we have a real
                // duration. Running builds always do (elapsed is
                // ticking); completed/failed ones do if `elapsed()`
                // is non-zero (live finish or persisted duration).
                // Older archives without timing data show zero and
                // hide the field.
                let has_duration =
                    matches!(build.status, BuildStatus::Running) || !build.elapsed().is_zero();
                if has_duration {
                    match &build.status {
                        BuildStatus::Running => {
                            let elapsed = format_duration(build.elapsed());
                            ui.label(
                                egui::RichText::new(elapsed)
                                    .size(18.0)
                                    .strong()
                                    .monospace()
                                    .color(theme::TEXT_PRIMARY),
                            );
                            ui.small(
                                egui::RichText::new("ELAPSED")
                                    .color(theme::TEXT_DIM)
                                    .monospace(),
                            );
                        }
                        BuildStatus::Succeeded { .. } => {
                            ui.label(
                                egui::RichText::new(format_duration(build.elapsed()))
                                    .monospace()
                                    .color(theme::TEXT_SECONDARY),
                            );
                            // Single label for both sources — the build
                            // happened, regardless of when we opened it.
                            ui.small(egui::RichText::new("built in").color(theme::TEXT_DIM));
                        }
                        BuildStatus::Failed { .. } => {
                            ui.label(
                                egui::RichText::new(format_duration(build.elapsed()))
                                    .monospace()
                                    .color(theme::TEXT_DIM),
                            );
                            ui.small(
                                egui::RichText::new("failed in").color(theme::TEXT_DIM),
                            );
                        }
                    }
                }
            });
        });
    });
    action
}

fn status_pill(ui: &mut egui::Ui, build: &BuildHandle) {
    let (label, col) = match &build.status {
        BuildStatus::Running => {
            let phase = build
                .phase
                .as_ref()
                .map(|p| format!("● {}", phase_live_label(p)))
                .unwrap_or_else(|| "● STARTING".into());
            (phase, theme::ACCENT)
        }
        BuildStatus::Succeeded { .. } => (format!("{} BUILT", icon::CHECK), theme::INFO),
        BuildStatus::Failed { .. } => (format!("{} FAILED", icon::X), theme::ERROR),
    };
    egui::Frame::NONE
        .fill(col.gamma_multiply(0.2))
        .stroke(egui::Stroke::new(1.0, col))
        .corner_radius(egui::CornerRadius::same(255))
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(label)
                    .monospace()
                    .strong()
                    .size(11.0)
                    .color(col),
            );
        });
}

fn phase_live_label(phase: &PipelinePhase) -> String {
    match phase {
        PipelinePhase::Planning => "PLANNING".into(),
        PipelinePhase::CompilingTasks => "COMPILING TASKS".into(),
        PipelinePhase::Organizing { .. } => "ORGANIZING".into(),
        PipelinePhase::CompilingApp => "COMPILING APP".into(),
        PipelinePhase::ExtractingMetadata => "EXTRACTING METADATA".into(),
        PipelinePhase::Packing => "PACKING".into(),
        PipelinePhase::Done => "DONE".into(),
    }
}

// ── Stats strip ─────────────────────────────────────────────────────────

fn stats_strip(ui: &mut egui::Ui, build: &BuildHandle) {
    let n_tasks = build
        .crates
        .iter()
        .filter(|c| c.kind == CrateKind::Task)
        .count();
    let n_hosts = build
        .crates
        .iter()
        .filter(|c| c.kind == CrateKind::HostCrate)
        .count();
    let cols: [(&str, egui::Color32, String, String); 4] = [
        (
            "APP",
            theme::ACCENT,
            build.config.config.clone(),
            format!("{n_tasks} task crates · {n_hosts} host"),
        ),
        (
            "BOARD",
            theme::WARN,
            build.config.board.clone(),
            String::new(),
        ),
        (
            "LAYOUT",
            theme::ACCENT,
            build.config.layout.clone(),
            format!("{} places", build.place_capacities.len()),
        ),
        (
            "PROFILE",
            theme::INFO,
            "release".into(),
            "opt 3 · LTO".into(),
        ),
    ];

    let bg = theme::BORDER_STRONG.gamma_multiply(0.2);
    egui::Frame::NONE
        .fill(bg)
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(0, 12))
        .show(ui, |ui| {
            // Use egui_taffy flexbox so the 4 columns stay equal width
            // and can't overflow the parent. Reserve only the available
            // WIDTH — letting the height size to content — so the strip
            // shrink-wraps the column labels instead of claiming the
            // whole pane's height.
            taffy_tui(ui, egui::Id::new(("stats_strip", build.id.0)))
                .reserve_available_width()
                .style(Style {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Row,
                    align_items: Some(AlignItems::Start),
                    size: Size {
                        width: percent(1.0),
                        height: auto(),
                    },
                    ..Default::default()
                })
                .show(|tui| {
                    let last = cols.len() - 1;
                    for (i, (label, label_col, value, sub)) in cols.iter().enumerate() {
                        tui.style(Style {
                            flex_grow: 1.0,
                            flex_basis: length(0.0),
                            min_size: Size {
                                width: length(0.0),
                                height: auto(),
                            },
                            padding: Rect {
                                left: length(16.0),
                                right: length(16.0),
                                top: length(0.0),
                                bottom: length(0.0),
                            },
                            display: Display::Flex,
                            flex_direction: FlexDirection::Column,
                            ..Default::default()
                        })
                        .ui(|ui| {
                            stat_col_content(ui, label, *label_col, value, sub);
                        });
                        if i != last {
                            tui.style(Style {
                                size: Size {
                                    width: length(1.0),
                                    height: length(42.0),
                                },
                                flex_shrink: 0.0,
                                ..Default::default()
                            })
                            .ui(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(1.0, 42.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(
                                    rect,
                                    egui::CornerRadius::ZERO,
                                    theme::SURFACE_BORDER,
                                );
                            });
                        }
                    }
                });
        });
}

fn stat_col_content(
    ui: &mut egui::Ui,
    label: &str,
    label_color: egui::Color32,
    value: &str,
    sub: &str,
) {
    ui.vertical(|ui| {
        ui.label(
            egui::RichText::new(label)
                .monospace()
                .strong()
                .size(9.0)
                .color(label_color),
        );
        ui.label(
            egui::RichText::new(value)
                .size(15.0)
                .monospace()
                .strong()
                .color(theme::TEXT_PRIMARY),
        );
        if !sub.is_empty() {
            ui.label(
                egui::RichText::new(sub)
                    .monospace()
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
        }
    });
}

// ── Pipeline stepper / trace ────────────────────────────────────────────

fn pipeline_section(ui: &mut egui::Ui, build: &BuildHandle) {
    let done = !matches!(build.status, BuildStatus::Running);
    if done {
        // Compact trace strip.
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("BUILD")
                        .monospace()
                        .strong()
                        .size(9.0)
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(8.0);
                let final_phase_order = build
                    .phase
                    .as_ref()
                    .map(|p| p.order())
                    .unwrap_or(6);
                let col_success = matches!(build.status, BuildStatus::Succeeded { .. });
                for (i, _phase) in PHASE_ORDER.iter().enumerate() {
                    let reached = i as u8 <= final_phase_order;
                    let col = if !reached {
                        theme::BORDER_STRONG
                    } else if col_success {
                        theme::INFO
                    } else {
                        theme::ERROR
                    };
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(10.0, 10.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().circle_filled(rect.center(), 5.0, col);
                    if i + 1 < PHASE_ORDER.len() {
                        let (lrect, _) = ui.allocate_exact_size(
                            egui::vec2(14.0, 2.0),
                            egui::Sense::hover(),
                        );
                        ui.painter().rect_filled(
                            lrect,
                            egui::CornerRadius::ZERO,
                            col,
                        );
                    }
                }
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let n_warn = build
                            .diagnostics
                            .iter()
                            .filter(|d| d.level == DiagnosticLevel::Warning)
                            .count();
                        let n_err = build
                            .diagnostics
                            .iter()
                            .filter(|d| d.level == DiagnosticLevel::Error)
                            .count();
                        if n_err > 0 {
                            ui.colored_label(
                                theme::ERROR,
                                format!("{} {n_err} errors", icon::X_CIRCLE),
                            );
                        }
                        if n_warn > 0 {
                            ui.colored_label(
                                theme::WARN,
                                format!("{} {n_warn} warnings", icon::WARNING),
                            );
                        }
                        ui.colored_label(
                            theme::TEXT_SECONDARY,
                            format!("in {}", format_duration(build.elapsed())),
                        );
                    },
                );
            });
        });
        return;
    }

    // Full stepper while running.
    card(ui, |ui| {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let current_order = build
                .phase
                .as_ref()
                .map(|p| p.order())
                .unwrap_or(0);
            let available = ui.available_width();
            let node_w = 60.0;
            let total_nodes_w = 7.0 * node_w;
            let remaining = available - total_nodes_w;
            let line_w = (remaining / 6.0).max(12.0);

            for (i, phase) in PHASE_ORDER.iter().enumerate() {
                let order_i = i as u8;
                let past = order_i < current_order;
                let current = order_i == current_order;

                let (circle_col, ring_col, icon_col, text_col, ring_w) = if past {
                    (
                        theme::INFO.gamma_multiply(0.2),
                        theme::INFO,
                        theme::INFO,
                        theme::INFO,
                        1.5,
                    )
                } else if current {
                    (
                        theme::ACCENT.gamma_multiply(0.25),
                        theme::ACCENT,
                        theme::ACCENT,
                        theme::ACCENT,
                        2.0,
                    )
                } else {
                    (
                        theme::BG,
                        theme::BORDER_STRONG,
                        theme::TEXT_DIM,
                        theme::TEXT_DIM,
                        1.5,
                    )
                };
                let size = if current { 34.0 } else { 28.0 };
                ui.allocate_ui_with_layout(
                    egui::vec2(node_w, 56.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(size, size),
                            egui::Sense::hover(),
                        );
                        ui.painter().circle_filled(rect.center(), size / 2.0, circle_col);
                        ui.painter().circle_stroke(
                            rect.center(),
                            size / 2.0,
                            egui::Stroke::new(ring_w, ring_col),
                        );
                        let glyph = if past {
                            icon::CHECK
                        } else {
                            phase_icon(phase)
                        };
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            glyph,
                            egui::FontId::proportional(size * 0.55),
                            icon_col,
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(phase.label())
                                .monospace()
                                .strong()
                                .size(10.0)
                                .color(text_col),
                        );
                    },
                );
                if i + 1 < PHASE_ORDER.len() {
                    // Connector line.
                    let next_past = (order_i + 1) < current_order;
                    let next_current = (order_i + 1) == current_order;
                    let line_col = if next_past {
                        theme::INFO
                    } else if past && next_current {
                        theme::ACCENT
                    } else {
                        theme::BORDER_STRONG
                    };
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(line_w, 2.0),
                        egui::Sense::hover(),
                    );
                    // Vertically center the line within the 56px row — stepper is top-aligned.
                    let center_y = rect.top() + 14.0;
                    let line_rect = egui::Rect::from_min_max(
                        egui::pos2(rect.left(), center_y - 1.0),
                        egui::pos2(rect.right(), center_y + 1.0),
                    );
                    ui.painter().rect_filled(
                        line_rect,
                        egui::CornerRadius::ZERO,
                        line_col,
                    );
                }
            }
        });
        ui.add_space(4.0);
    });
}

// ── Crates card ─────────────────────────────────────────────────────────

fn crates_card(ui: &mut egui::Ui, build: &BuildHandle) {
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(theme::ACCENT, icon::STACK);
            ui.label(
                egui::RichText::new("CRATES")
                    .monospace()
                    .strong()
                    .size(13.0)
                    .color(theme::TEXT_PRIMARY),
            );
            let n_total = build.crates.len();
            let n_linked = build
                .crates
                .iter()
                .filter(|c| c.state == CrateBuildState::Linked)
                .count();
            let n_inflight = build
                .crates
                .iter()
                .filter(|c| matches!(
                    c.state,
                    CrateBuildState::Building
                        | CrateBuildState::Measuring
                        | CrateBuildState::Linking
                ))
                .count();
            let n_queued = build
                .crates
                .iter()
                .filter(|c| c.state == CrateBuildState::Queued)
                .count();
            let n_failed = build
                .crates
                .iter()
                .filter(|c| c.state == CrateBuildState::Failed)
                .count();
            let summary = if n_total == 0 {
                "no crates yet".to_string()
            } else {
                let mut parts = Vec::new();
                if n_linked > 0 {
                    parts.push(format!("{n_linked} / {n_total} linked"));
                }
                if n_inflight > 0 {
                    parts.push(format!("{n_inflight} in flight"));
                }
                if n_queued > 0 {
                    parts.push(format!("{n_queued} queued"));
                }
                if n_failed > 0 {
                    parts.push(format!("{n_failed} failed"));
                }
                parts.join("  ·  ")
            };
            ui.colored_label(theme::TEXT_DIM, summary);
        });
        ui.add_space(6.0);

        let has_any = |k: CrateKind| build.crates.iter().any(|c| c.kind == k);

        if has_any(CrateKind::Bootloader) {
            subheader(ui, "BOOTLOADER", theme::ACCENT);
            for c in build.crates_by_kind(CrateKind::Bootloader) {
                crate_row(ui, build, c);
            }
        }
        if has_any(CrateKind::Kernel) {
            subheader(ui, "KERNEL", theme::WARN);
            for c in build.crates_by_kind(CrateKind::Kernel) {
                crate_row(ui, build, c);
            }
        }
        if has_any(CrateKind::Task) {
            subheader(ui, "TASKS", theme::INFO);
            for c in build.crates_by_kind(CrateKind::Task) {
                crate_row(ui, build, c);
            }
        }
        if has_any(CrateKind::Sysmodule) {
            subheader(ui, "SYSMODULES", theme::SYSMOD);
            for c in build.crates_by_kind(CrateKind::Sysmodule) {
                crate_row(ui, build, c);
            }
        }
        if has_any(CrateKind::HostCrate) {
            subheader(ui, "HOST CRATES", theme::HOST);
            for c in build.crates_by_kind(CrateKind::HostCrate) {
                crate_row(ui, build, c);
            }
        }

        if build.crates.is_empty() {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                ui.colored_label(theme::TEXT_DIM, "waiting for build to start…");
            });
            ui.add_space(8.0);
        }
    });
}

fn subheader(ui: &mut egui::Ui, label: &str, color: egui::Color32) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(label)
                .monospace()
                .strong()
                .size(9.0)
                .color(color),
        );
    });
    ui.add_space(2.0);
}

fn crate_icon(kind: CrateKind) -> &'static str {
    match kind {
        CrateKind::Bootloader => icon::ROCKET_LAUNCH,
        CrateKind::Kernel => icon::SHIELD_CHECK,
        CrateKind::Task => icon::GEAR,
        CrateKind::Sysmodule => icon::CUBE,
        CrateKind::HostCrate => icon::FLASK,
    }
}

fn crate_kind_color(kind: CrateKind) -> egui::Color32 {
    match kind {
        CrateKind::Bootloader => theme::ACCENT,
        CrateKind::Kernel => theme::WARN,
        CrateKind::Task => theme::INFO,
        CrateKind::Sysmodule => theme::SYSMOD,
        CrateKind::HostCrate => theme::HOST,
    }
}

/// Render a single crate row. Uses egui's `CollapsingState::show_header`
/// for a real clickable header with a chevron; the row force-opens
/// while a crate is actively Building (so the cargo log is visible)
/// or Failed (so the error stays in view).
fn crate_row(ui: &mut egui::Ui, build: &BuildHandle, c: &CrateProgress) {
    let failed = c.state == CrateBuildState::Failed;
    let building = c.state == CrateBuildState::Building;
    let has_ipc = !c.provides.is_empty() || !c.uses.is_empty();
    // The row earns a dropdown if it has:
    //   - an active cargo log to stream
    //   - a failure to explain
    //   - IPC metadata (provides/uses) once linked
    let has_body = failed || !c.cargo_log.is_empty() || has_ipc;
    let default_open = failed || building;

    let border = if failed {
        Some(theme::ERROR)
    } else if matches!(
        c.state,
        CrateBuildState::Building
            | CrateBuildState::Compiled
            | CrateBuildState::Measuring
            | CrateBuildState::Linking
    ) {
        Some(theme::ACCENT)
    } else {
        None
    };
    let opacity = if c.state == CrateBuildState::Queued {
        0.55
    } else {
        1.0
    };

    ui.add_space(3.0);

    // Layered fills matching the pencil design (dark → darkest → dark-ish):
    // - Parent CRATES card is `theme::SURFACE` (lightest).
    // - The dropdown row's outer frame is `theme::BG` (darkest), with
    //   optional colour-coded border for in-flight / failed states.
    // - Inside, the header has no fill (shows through to BG). The body,
    //   when open, paints `theme::PANEL` on top — one notch lighter
    //   than BG so the expanded body is visually distinct from the
    //   header row without reading as a separate nested card.
    let mut outer_frame = egui::Frame::NONE
        .fill(theme::BG)
        .corner_radius(egui::CornerRadius::same(5));
    if let Some(col) = border {
        outer_frame = outer_frame.stroke(egui::Stroke::new(1.0, col));
    }

    ui.scope(|ui| {
        ui.set_opacity(opacity);

        if has_body {
            let id = ui.make_persistent_id((
                "crate_row",
                build.id.0,
                c.name.as_str(),
            ));
            let mut state =
                egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    id,
                    default_open,
                );
            // Force open while actively building or failed — the log /
            // error is too important to hide.
            if (building && !c.cargo_log.is_empty()) || failed {
                state.set_open(true);
            }
            let is_open = state.is_open();

            outer_frame.show(ui, |ui| {
                // Header (inside the outer BG frame). No own fill so
                // the outer's BG shows through — only padding here.
                let chevron = if is_open {
                    icon::CARET_DOWN
                } else {
                    icon::CARET_RIGHT
                };
                let header_inner = egui::Frame::NONE
                    .inner_margin(egui::Margin {
                        left: 10,
                        right: 10,
                        top: 6,
                        bottom: 6,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.small(
                                egui::RichText::new(chevron).color(theme::TEXT_DIM),
                            );
                            ui.add_space(4.0);
                            crate_row_header_content(ui, c);
                        });
                    });
                let click = ui.interact(
                    header_inner.response.rect,
                    id.with("header_click"),
                    egui::Sense::click(),
                );
                if click.clicked() {
                    state.toggle(ui);
                }

                // Body — painted inside the outer frame, with its own
                // `PANEL` fill so it reads as a distinct block below
                // the header. Bottom corners rounded to match the
                // outer frame's radius.
                state.show_body_unindented(ui, |ui| {
                    // Body lives entirely in taffy: the root node
                    // claims full available width, and a single child
                    // node owns the native taffy padding + paints the
                    // PANEL fill against its own allocated rect via
                    // `add_with_background_ui`. No egui::Frame — taffy
                    // does the layout, we just paint on top of the
                    // taffy-assigned rect.
                    taffy_tui(
                        ui,
                        egui::Id::new((
                            "crate_row_body",
                            build.id.0,
                            c.name.as_str(),
                        )),
                    )
                    .reserve_available_width()
                    .style(Style {
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        size: Size {
                            width: percent(1.0),
                            height: auto(),
                        },
                        ..Default::default()
                    })
                    .show(|tui| {
                        tui.style(Style {
                            display: Display::Flex,
                            flex_direction: FlexDirection::Column,
                            size: Size {
                                width: percent(1.0),
                                height: auto(),
                            },
                            padding: Rect {
                                left: length(18.0),
                                right: length(10.0),
                                top: length(8.0),
                                bottom: length(8.0),
                            },
                            ..Default::default()
                        })
                        .add_with_background_ui(
                            |ui, container| {
                                ui.painter().rect_filled(
                                    container.full_container(),
                                    egui::CornerRadius {
                                        nw: 0,
                                        ne: 0,
                                        sw: 5,
                                        se: 5,
                                    },
                                    theme::PANEL,
                                );
                            },
                            |tui, _| {
                                tui.style(Style {
                                    size: Size {
                                        width: percent(1.0),
                                        height: auto(),
                                    },
                                    ..Default::default()
                                })
                                .ui(|ui| crate_row_body(ui, c));
                            },
                        );
                    });
                });
            });
        } else {
            // No body — just the flat outer pill, chevron-sized
            // spacer so text lines up with rows that do have one.
            outer_frame
                .inner_margin(egui::Margin {
                    left: 10,
                    right: 10,
                    top: 6,
                    bottom: 6,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        crate_row_header_content(ui, c);
                    });
                });
        }
    });
}

/// Right-hand bulk of a crate row — rendered inside either the
/// collapsing header (if the row has a body) or a plain horizontal
/// layout (if it doesn't).
fn crate_row_header_content(ui: &mut egui::Ui, c: &CrateProgress) {
    let kind_col = match c.state {
        CrateBuildState::Failed => theme::ERROR,
        _ => crate_kind_color(c.kind),
    };
    ui.colored_label(kind_col, crate_icon(c.kind));

    let name_col = match c.state {
        CrateBuildState::Queued => theme::TEXT_SECONDARY,
        _ => match c.kind {
            CrateKind::Bootloader => theme::ACCENT,
            CrateKind::Kernel => theme::WARN,
            CrateKind::Sysmodule => theme::SYSMOD,
            _ => theme::TEXT_PRIMARY,
        },
    };
    ui.label(
        egui::RichText::new(&c.name)
            .monospace()
            .strong()
            .size(12.0)
            .color(name_col),
    );
    if c.supervisor {
        egui::Frame::NONE
            .fill(theme::WARN.gamma_multiply(0.2))
            .corner_radius(egui::CornerRadius::same(3))
            .inner_margin(egui::Margin::symmetric(6, 2))
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("SUPERVISOR")
                        .monospace()
                        .strong()
                        .size(9.0)
                        .color(theme::WARN),
                );
            });
    }

    // Right-aligned state info.
    ui.with_layout(
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            let right_text = right_side_label(c);
            let right_col = match c.state {
                CrateBuildState::Queued => theme::TEXT_DIM,
                CrateBuildState::Linked => theme::TEXT_DIM,
                CrateBuildState::Failed => theme::ERROR,
                _ => theme::ACCENT,
            };
            ui.label(
                egui::RichText::new(right_text)
                    .monospace()
                    .size(10.0)
                    .color(right_col),
            );
            let wg = if c.kind == CrateKind::HostCrate {
                "host"
            } else {
                "main"
            };
            egui::Frame::NONE
                .fill(theme::PANEL)
                .corner_radius(egui::CornerRadius::same(3))
                .inner_margin(egui::Margin::symmetric(7, 2))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(wg)
                            .monospace()
                            .strong()
                            .size(9.0)
                            .color(theme::TEXT_SECONDARY),
                    );
                });
        },
    );
}

/// Right-side text for a crate row — state + pip cluster / size.
/// The pip cluster mirrors progression through the four mid-build
/// states (`Building → Compiled → Measuring → Linking`) so users can
/// see how far along a crate is at a glance; `Linked` drops the pips
/// and shows the ELF size.
fn right_side_label(c: &CrateProgress) -> String {
    let pri = c.priority.map(|p| format!("pri {p}")).unwrap_or_default();
    let pri_prefix = if pri.is_empty() {
        String::new()
    } else {
        format!("{pri}  ·  ")
    };
    match c.state {
        CrateBuildState::Queued => "queued".to_string(),
        CrateBuildState::Building => format!("{pri_prefix}●○○○ building"),
        CrateBuildState::Compiled => format!("{pri_prefix}●●○○ compiled"),
        CrateBuildState::Measuring => format!("{pri_prefix}●●●○ measuring"),
        CrateBuildState::Linking => format!("{pri_prefix}●●●● linking"),
        CrateBuildState::Linked => {
            let size = c
                .total_size
                .map(|s| format!("  ·  {}", format_bytes(s)))
                .unwrap_or_default();
            format!("{pri}{size}")
        }
        CrateBuildState::Failed => format!("{pri_prefix}build failed"),
    }
}

fn crate_row_body(ui: &mut egui::Ui, c: &CrateProgress) {
    // No frame here — the caller already handles padding via its own
    // `Frame::inner_margin`. Leaving the body "unwrapped" lets it
    // blend into the parent CRATES card's surface instead of looking
    // like a nested card.
    {
        ui.vertical(|ui| {
                // Cargo log (streaming during Building, frozen on Failed).
                for line in c.cargo_log.iter().take(40) {
                    for sub in line.lines().take(8) {
                        ui.label(
                            egui::RichText::new(sub)
                                .monospace()
                                .size(11.0)
                                .color(theme::TEXT_SECONDARY),
                        );
                    }
                }
                if c.cargo_log.is_empty() && c.state == CrateBuildState::Failed {
                    if let Some(err) = &c.error {
                        ui.colored_label(theme::ERROR, err);
                    }
                }

                // IPC metadata — shown once we have it (after Pack
                // extracts schemas, or when rendering a loaded snapshot).
                let need_sep = !c.cargo_log.is_empty()
                    && (!c.provides.is_empty() || !c.uses.is_empty());
                if need_sep {
                    ui.add_space(4.0);
                }
                if !c.provides.is_empty() {
                    ipc_chip_row(
                        ui,
                        "provides",
                        c.provides.iter().map(|p| ChipSpec::pill(p.resource.clone())),
                    );
                }
                if !c.uses.is_empty() {
                    ipc_chip_row(
                        ui,
                        "uses    ",
                        c.uses.iter().map(|u| {
                            let text = if u.resource.is_empty() {
                                u.server_task.clone()
                            } else {
                                format!("{}::{}", u.server_task, u.resource)
                            };
                            ChipSpec::boxy(text)
                        }),
                    );
                }
            });
    }
}

/// Chip variant. `Pill` matches provided resources (accent-filled,
/// soft pill). `Boxy` matches consumed resources (monospace code
/// identifier in a bordered box).
enum ChipSpec {
    Pill(String),
    Boxy(String),
}

impl ChipSpec {
    fn pill(s: impl Into<String>) -> Self {
        ChipSpec::Pill(s.into())
    }
    fn boxy(s: impl Into<String>) -> Self {
        ChipSpec::Boxy(s.into())
    }
}

fn ipc_chip_row(
    ui: &mut egui::Ui,
    label: &str,
    chips: impl IntoIterator<Item = ChipSpec>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(label)
                .monospace()
                .strong()
                .size(9.0)
                .color(theme::TEXT_DIM),
        );
        ui.add_space(4.0);
        for chip in chips {
            match chip {
                ChipSpec::Pill(text) => {
                    egui::Frame::NONE
                        .fill(theme::ACCENT.gamma_multiply(0.2))
                        .corner_radius(egui::CornerRadius::same(255))
                        .inner_margin(egui::Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(text)
                                    .monospace()
                                    .strong()
                                    .size(10.0)
                                    .color(theme::ACCENT),
                            );
                        });
                }
                ChipSpec::Boxy(text) => {
                    egui::Frame::NONE
                        .fill(theme::BG)
                        .stroke(egui::Stroke::new(1.0, theme::SURFACE_BORDER))
                        .corner_radius(egui::CornerRadius::same(3))
                        .inner_margin(egui::Margin::symmetric(7, 2))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(text)
                                    .monospace()
                                    .size(11.0)
                                    .color(theme::TEXT_SECONDARY),
                            );
                        });
                }
            }
        }
    });
}

// ── Memory map ──────────────────────────────────────────────────────────

fn memory_card(ui: &mut egui::Ui, build: &BuildHandle) {
    if build.memories.is_empty() {
        // No physical memory info known yet — skip rendering rather
        // than show an ambiguous empty box.
        return;
    }

    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(theme::ACCENT, icon::HARD_DRIVES);
            ui.label(
                egui::RichText::new("MEMORY MAP")
                    .monospace()
                    .strong()
                    .size(13.0)
                    .color(theme::TEXT_PRIMARY),
            );
            ui.colored_label(
                theme::TEXT_DIM,
                format!("{} devices", build.memories.len()),
            );
        });
        ui.add_space(6.0);

        for dev in &build.memories {
            memory_device_row(ui, build, dev);
            ui.add_space(8.0);
        }
    });
}

/// Render a single memory device bar. Allocations whose base address
/// falls inside one of the device's CPU mappings count toward its
/// "used" portion, coloured by the owner's crate kind.
fn memory_device_row(
    ui: &mut egui::Ui,
    build: &BuildHandle,
    dev: &crate::state::MemoryDevice,
) {
    // Which allocations live inside this device?
    let allocs: Vec<&MemoryAllocation> = build
        .allocations
        .iter()
        .filter(|a| dev.contains_address(a.base))
        .collect();
    let used: u64 = allocs.iter().map(|a| a.size).sum();
    let capacity = dev.size;
    let pct = if capacity > 0 {
        Some((used as f64 / capacity as f64 * 100.0).round() as u32)
    } else {
        None
    };

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(&dev.name)
                .monospace()
                .strong()
                .size(11.0)
                .color(theme::TEXT_PRIMARY),
        );
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let text = match pct {
                    Some(p) => format!(
                        "{} / {}  ·  {}%",
                        format_bytes(used),
                        format_bytes(capacity),
                        p
                    ),
                    None => format!("{} / {}", format_bytes(used), format_bytes(capacity)),
                };
                ui.small(
                    egui::RichText::new(text)
                        .monospace()
                        .color(theme::TEXT_SECONDARY),
                );
            },
        );
    });

    let bar_w = ui.available_width();
    let (track_rect, _) = ui.allocate_exact_size(
        egui::vec2(bar_w, 8.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(
        track_rect,
        egui::CornerRadius::same(3),
        theme::BG,
    );

    if capacity == 0 {
        return;
    }
    // Draw per-allocation segments at their true proportional scale
    // against the device's capacity — so the unused portion is
    // correctly visible on the right.
    let mut x = track_rect.left();
    for a in &allocs {
        let w = (a.size as f32 / capacity as f32) * track_rect.width();
        let seg = egui::Rect::from_min_max(
            egui::pos2(x, track_rect.top()),
            egui::pos2((x + w).min(track_rect.right()), track_rect.bottom()),
        );
        let col = owner_color(&a.owner);
        ui.painter()
            .rect_filled(seg, egui::CornerRadius::same(3), col);
        x += w;
    }
    // Attention border when we get close to capacity.
    if let Some(p) = pct {
        if p >= 85 {
            let used_w = (used as f32 / capacity as f32).min(1.0) * track_rect.width();
            let warn_rect = egui::Rect::from_min_max(
                egui::pos2(track_rect.left(), track_rect.top()),
                egui::pos2(track_rect.left() + used_w, track_rect.bottom()),
            );
            ui.painter().rect_stroke(
                warn_rect,
                egui::CornerRadius::same(3),
                egui::Stroke::new(1.0, theme::WARN),
                egui::StrokeKind::Inside,
            );
        }
    }

    // Subline: base address + mapping count — helps distinguish between
    // same-named devices (`flash_text`, `flash_data`) that live at
    // different addresses.
    if let Some((addr, _)) = dev.mappings.first() {
        ui.horizontal(|ui| {
            ui.small(
                egui::RichText::new(format!("@ {:#010x}", addr))
                    .monospace()
                    .color(theme::TEXT_DIM),
            );
            if dev.mappings.len() > 1 {
                ui.small(
                    egui::RichText::new(format!(
                        "·  {} mappings",
                        dev.mappings.len()
                    ))
                    .monospace()
                    .color(theme::TEXT_DIM),
                );
            }
        });
    }
}

fn owner_color(owner: &str) -> egui::Color32 {
    match owner {
        "kernel" => theme::WARN,
        "bootloader" => theme::ACCENT,
        _ => theme::INFO,
    }
}

// ── Footer ──────────────────────────────────────────────────────────────

/// Bottom-of-panel footer — thin single-line strip with image / archive
/// metadata. Replaces the previous IMAGE card; lives outside the
/// two-column body so it always stretches full-width.
fn footer(ui: &mut egui::Ui, build: &BuildHandle) {
    let bg = theme::BORDER_STRONG.gamma_multiply(0.2);
    egui::Frame::NONE
        .fill(bg)
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(16, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // Left: image status / icon + size summary.
                match &build.image {
                    ImageProgress::None => {
                        ui.colored_label(theme::TEXT_DIM, icon::HOURGLASS_MEDIUM);
                        ui.label(
                            egui::RichText::new("awaiting pack")
                                .monospace()
                                .size(11.0)
                                .color(theme::TEXT_DIM),
                        );
                    }
                    ImageProgress::Assembled { size } => {
                        ui.colored_label(theme::ACCENT, icon::PACKAGE);
                        ui.label(
                            egui::RichText::new(format!("assembled · {}", format_bytes(*size)))
                                .monospace()
                                .size(11.0)
                                .color(theme::ACCENT),
                        );
                    }
                    ImageProgress::Archived { size, .. } => {
                        ui.colored_label(theme::INFO, icon::PACKAGE);
                        ui.label(
                            egui::RichText::new(format!("{} · {}", icon::CHECK, format_bytes(*size)))
                                .monospace()
                                .size(11.0)
                                .color(theme::INFO),
                        );
                    }
                }
                // Right: truncated archive path with full-path tooltip.
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if let ImageProgress::Archived { path, .. } = &build.image {
                            let p = path.display().to_string();
                            let label = egui::Label::new(
                                egui::RichText::new(shorten_path(&p))
                                    .monospace()
                                    .size(10.0)
                                    .color(theme::TEXT_DIM),
                            )
                            .truncate();
                            ui.add(label).on_hover_text(p);
                        }
                    },
                );
            });
        });
}

/// Shorten a long path for single-line display: keep the last component
/// preceded by a `…/` when the path has more than 3 segments.
fn shorten_path(path: &str) -> String {
    // Normalise separators so the split below works on Windows & Unix.
    let sep = std::path::MAIN_SEPARATOR;
    let parts: Vec<&str> = path.split(sep).filter(|s| !s.is_empty()).collect();
    if parts.len() <= 3 {
        return path.to_string();
    }
    let last = parts[parts.len() - 1];
    let parent = parts[parts.len() - 2];
    format!("…{sep}{parent}{sep}{last}")
}

// ── Diagnostics ─────────────────────────────────────────────────────────

fn diagnostics_section(ui: &mut egui::Ui, build: &BuildHandle) {
    let warnings: Vec<&Diagnostic> = build
        .diagnostics
        .iter()
        .filter(|d| d.level == DiagnosticLevel::Warning)
        .collect();
    let errors: Vec<&Diagnostic> = build
        .diagnostics
        .iter()
        .filter(|d| d.level == DiagnosticLevel::Error)
        .collect();
    if warnings.is_empty() && errors.is_empty() {
        return;
    }
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(theme::WARN, icon::WARNING_CIRCLE);
            ui.label(
                egui::RichText::new("DIAGNOSTICS")
                    .monospace()
                    .strong()
                    .size(13.0)
                    .color(theme::TEXT_PRIMARY),
            );
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    pill(
                        ui,
                        theme::ERROR,
                        &format!("{} ERRORS", errors.len()),
                    );
                    pill(
                        ui,
                        theme::WARN,
                        &format!("{} WARNINGS", warnings.len()),
                    );
                },
            );
        });
        ui.add_space(6.0);
        // Show errors first (most important).
        for d in errors.iter().take(6) {
            diagnostic_row(ui, d);
        }
        for d in warnings.iter().take(6) {
            diagnostic_row(ui, d);
        }
        if errors.len() + warnings.len() > 12 {
            ui.small(
                egui::RichText::new(format!(
                    "{} ... {} more",
                    icon::CARET_DOWN,
                    errors.len() + warnings.len() - 12
                ))
                .color(theme::TEXT_SECONDARY),
            );
        }
    });
}

fn pill(ui: &mut egui::Ui, color: egui::Color32, label: &str) {
    egui::Frame::NONE
        .fill(color.gamma_multiply(0.18))
        .corner_radius(egui::CornerRadius::same(255))
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(label)
                    .monospace()
                    .strong()
                    .size(10.0)
                    .color(color),
            );
        });
}

fn diagnostic_row(ui: &mut egui::Ui, d: &Diagnostic) {
    let (stripe, level_text) = match d.level {
        DiagnosticLevel::Error => (theme::ERROR, "error"),
        DiagnosticLevel::Warning => (theme::WARN, "warning"),
        DiagnosticLevel::Note => (theme::ACCENT, "note"),
        DiagnosticLevel::Help => (theme::INFO, "help"),
    };
    egui::Frame::NONE
        .fill(theme::BG)
        .corner_radius(egui::CornerRadius::same(6))
        .stroke(egui::Stroke::new(1.0, stripe.gamma_multiply(0.25)))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // Left stripe.
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(3.0, 34.0), egui::Sense::hover());
                ui.painter().rect_filled(
                    rect,
                    egui::CornerRadius::same(2),
                    stripe,
                );
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            stripe,
                            egui::RichText::new(level_text).monospace().strong().size(11.0),
                        );
                        ui.colored_label(
                            theme::TEXT_SECONDARY,
                            egui::RichText::new(&d.crate_name).monospace().size(11.0),
                        );
                    });
                    // Render a single-line summary — first non-empty line.
                    let summary = d
                        .rendered
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .trim();
                    ui.label(
                        egui::RichText::new(summary)
                            .monospace()
                            .size(12.0)
                            .color(theme::TEXT_PRIMARY),
                    );
                });
            });
        });
    ui.add_space(4.0);
}

// ── Pipeline log (collapsed) ────────────────────────────────────────────

fn pipeline_log(ui: &mut egui::Ui, build: &BuildHandle) {
    if build.log.is_empty() {
        return;
    }
    let id = ui.make_persistent_id(("pipeline_log", build.id.0));
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
        .show_header(ui, |ui| {
            ui.colored_label(theme::TEXT_SECONDARY, icon::TERMINAL);
            ui.label(
                egui::RichText::new("PIPELINE LOG")
                    .monospace()
                    .strong()
                    .size(13.0)
                    .color(theme::TEXT_SECONDARY),
            );
            ui.colored_label(
                theme::TEXT_DIM,
                format!("{} lines · stage events", build.log.len()),
            );
        })
        .body(|ui| {
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .auto_shrink([false, true])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &build.log {
                        ui.label(
                            egui::RichText::new(line)
                                .monospace()
                                .size(11.0)
                                .color(theme::TEXT_SECONDARY),
                        );
                    }
                });
        });
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn card<R>(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    // Lock card width to the ui's available width so inner content
    // can't push the card wider than its column. `set_min_width` alone
    // leaves the card free to grow past the boundary when a label is
    // slightly too wide — `set_width` clamps both directions.
    egui::Frame::NONE
        .fill(theme::SURFACE)
        .stroke(egui::Stroke::new(1.0, theme::SURFACE_BORDER))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(18, 14))
        .show(ui, |ui| {
            let w = ui.available_width();
            ui.set_width(w);
            ui.set_max_width(w);
            content(ui)
        })
        .inner
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let tenths = (d.subsec_millis() / 100) as u64;
    format!("{:02}:{:02}.{}", secs / 60, secs % 60, tenths)
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
