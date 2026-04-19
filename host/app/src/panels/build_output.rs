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
use egui_taffy::{TuiBuilderLogic, tui as taffy_tui};

use crate::state::{
    BuildHandle, BuildStatus, CargoDiagLevel, CargoDiagnostic, CrateBuildState, CrateKind,
    CrateProgress, ImageProgress, MemoryAllocation, PipelinePhase,
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
            let w = ui.available_width();
            ui.set_max_width(w);
            taffy_tui(ui, egui::Id::new("panel_root"))
                .reserve_available_width()
                .style(Style {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    gap: Size {
                        width: length(0.0),
                        height: length(12.0),
                    },
                    padding: Rect {
                        left: length(18.0),
                        right: length(18.0),
                        top: length(14.0),
                        bottom: length(14.0),
                    },
                    size: Size {
                        width: percent(1.0),
                        height: auto(),
                    },
                    ..Default::default()
                })
                .show(|tui| {
                    tui.style(Style::default()).ui(|ui| {
                        action = hero(ui, build);
                    });
                    tui.style(Style::default()).ui(|ui| {
                        pipeline_section(ui, build);
                    });
                    tui.style(Style::default()).ui(|ui| {
                        two_column_body(ui, build);
                    });
                    tui.style(Style::default()).ui(|ui| {
                        diagnostics_section(ui, build);
                    });
                });
        });
    action
}

fn two_column_body(ui: &mut egui::Ui, build: &BuildHandle) {
    let fixed_w: f32 = 380.0;
    let gap = 12.0;
    // On narrow viewports collapse to a single column.
    if ui.available_width() < fixed_w * 2.5 {
        memory_card(ui, build);
        ui.add_space(gap);
        crates_card(ui, build);
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
            // Left column — memory map, grows to fill.
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
                memory_card(ui, build);
            });
            // Right column — crates + resources, fixed width.
            tui.style(Style {
                flex_shrink: 0.0,
                size: Size {
                    width: length(fixed_w),
                    height: auto(),
                },
                ..Default::default()
            })
            .ui(|ui| {
                crates_card(ui, build);
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
                                egui::Id::new(("resource_row_body", build.id.0, res.name.as_str())),
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
                                                        .color(theme::TEXT_SECONDARY),
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
    let display_name = build.name.as_deref().unwrap_or(&build.config.config);

    taffy_tui(ui, egui::Id::new(("hero", build.id.0)))
        .reserve_available_width()
        .style(Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Row,
            align_items: Some(AlignItems::Center),
            padding: Rect {
                left: length(0.0),
                right: length(0.0),
                top: length(0.0),
                bottom: length(0.0),
            },
            gap: Size {
                width: length(12.0),
                height: length(0.0),
            },
            size: Size {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        })
        .show(|tui| {
            // ── Left: vertical stack of title row + config row ─────
            tui.style(Style {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: length(0.0),
                min_size: Size {
                    width: length(0.0),
                    height: auto(),
                },
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                gap: Size {
                    width: length(0.0),
                    height: length(8.0),
                },
                ..Default::default()
            })
            .add_with_background_ui(
                |_, _| {},
                |tui, _| {
                    // Title row: pill + name
                    tui.style(Style {
                        display: Display::Flex,
                        flex_direction: FlexDirection::Row,
                        align_items: Some(AlignItems::Center),
                        gap: Size {
                            width: length(6.0),
                            height: length(0.0),
                        },
                        ..Default::default()
                    })
                    .add_with_background_ui(
                        |_, _| {},
                        |tui, _| {
                            tui.style(Style::default()).ui(|ui| {
                                status_pill(ui, build);
                            });
                            tui.style(Style::default()).ui(|ui| {
                                ui.label(
                                    egui::RichText::new(display_name)
                                        .size(22.0)
                                        .strong()
                                        .color(theme::TEXT_PRIMARY),
                                );
                            });
                        },
                    );
                    // Config row: file paths
                    tui.style(Style {
                        display: Display::Flex,
                        flex_direction: FlexDirection::Row,
                        flex_wrap: FlexWrap::Wrap,
                        align_items: Some(AlignItems::Center),
                        gap: Size {
                            width: length(12.0),
                            height: length(3.0),
                        },
                        ..Default::default()
                    })
                    .add_with_background_ui(
                        |_, _| {},
                        |tui, _| {
                            // Config/board/layout chips
                            for (glyph, value) in [
                                (icon::FILE_CODE, build.config.config.as_str()),
                                (icon::CPU, build.config.board.as_str()),
                                (icon::LAYOUT, build.config.layout.as_str()),
                            ] {
                                hero_chip(tui, glyph, value);
                            }
                            // UUID chip
                            if let Some(uuid) = &build.uuid {
                                hero_chip(tui, icon::FINGERPRINT, uuid);
                            }
                        },
                    );
                },
            );

            // ── Right: timer + buttons ────────────────────────────
            tui.style(Style {
                flex_shrink: 1.0,
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                align_items: Some(AlignItems::Center),
                gap: Size {
                    width: length(6.0),
                    height: length(0.0),
                },
                ..Default::default()
            })
            .add_with_background_ui(
                |_, _| {},
                |tui, _| {
                    let has_file = matches!(
                        build.image,
                        ImageProgress::Archived { .. }
                    );
                    if has_file {
                        if let BuildStatus::Succeeded {
                            firmware_id: Some(fw_id),
                            ..
                        } = &build.status
                        {
                            tui.style(Style::default()).ui(|ui| {
                                let emu = ui.add(
                                    egui::Button::new(
                                        egui::RichText::new(format!("{}  Emulate", icon::PLAY))
                                            .size(13.0)
                                            .color(theme::TEXT_PRIMARY),
                                    )
                                    .fill(egui::Color32::TRANSPARENT)
                                    .stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
                                    .corner_radius(egui::CornerRadius::same(6))
                                    .min_size(egui::vec2(100.0, 32.0)),
                                );
                                if emu.clicked() {
                                    action = PanelAction::RunEmulator(*fw_id);
                                }
                            });
                            tui.style(Style::default()).ui(|ui| {
                                let flash = ui.add(
                                    egui::Button::new(
                                        egui::RichText::new(format!("{}  Flash", icon::LIGHTNING))
                                            .strong()
                                            .size(13.0)
                                            .color(theme::BG),
                                    )
                                    .fill(theme::ACCENT)
                                    .corner_radius(egui::CornerRadius::same(6))
                                    .min_size(egui::vec2(100.0, 32.0)),
                                );
                                if flash.clicked() {
                                    action = PanelAction::Flash(*fw_id);
                                }
                            });
                        }
                    }
                    if has_file && !matches!(build.status, BuildStatus::Running) {
                        tui.style(Style::default()).ui(|ui| {
                            let del = ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new(icon::TRASH)
                                            .size(16.0)
                                            .color(theme::ERROR),
                                    )
                                    .fill(egui::Color32::TRANSPARENT)
                                    .stroke(egui::Stroke::new(
                                        1.0,
                                        theme::ERROR.gamma_multiply(0.4),
                                    ))
                                    .corner_radius(egui::CornerRadius::same(6))
                                    .min_size(egui::vec2(32.0, 32.0)),
                                )
                                .on_hover_text("Delete this build record");
                            if del.clicked() {
                                action = PanelAction::DeleteBuild(build.id);
                            }
                        });
                    }
                },
            );
        });
    action
}

/// A single icon+text chip in the hero's info row.
fn hero_chip(tui: &mut egui_taffy::Tui, glyph: &str, value: &str) {
    tui.style(Style {
        display: Display::Flex,
        flex_direction: FlexDirection::Row,
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(3.0),
            height: length(0.0),
        },
        ..Default::default()
    })
    .add_with_background_ui(
        |_, _| {},
        |tui, _| {
            tui.style(Style::default()).ui(|ui| {
                ui.colored_label(theme::TEXT_DIM, glyph);
            });
            tui.style(Style::default()).ui(|ui| {
                ui.label(
                    egui::RichText::new(value)
                        .monospace()
                        .size(11.0)
                        .color(theme::TEXT_SECONDARY),
                );
            });
        },
    );
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
        BuildStatus::Succeeded { .. } => ("BUILT".into(), theme::INFO),
        BuildStatus::Failed { .. } => ("FAILED".into(), theme::ERROR),
    };
    egui::Frame::NONE
        .fill(col.gamma_multiply(0.2))
        .stroke(egui::Stroke::new(1.0, col))
        .corner_radius(egui::CornerRadius::same(255))
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                // Icon in proportional font so Phosphor glyphs render.
                if !matches!(build.status, BuildStatus::Running) {
                    let glyph = match &build.status {
                        BuildStatus::Succeeded { .. } => icon::CHECK,
                        BuildStatus::Failed { .. } => icon::X,
                        _ => "",
                    };
                    ui.label(egui::RichText::new(glyph).size(11.0).color(col));
                }
                ui.label(
                    egui::RichText::new(label)
                        .monospace()
                        .strong()
                        .size(11.0)
                        .color(col),
                );
            });
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

// ── Pipeline stepper / trace ────────────────────────────────────────────

fn pipeline_section(ui: &mut egui::Ui, build: &BuildHandle) {
    let done = !matches!(build.status, BuildStatus::Running);
    if done {
        // Quiet metadata line — no card, just dim text.
        ui.horizontal_wrapped(|ui| {
            let mut parts: Vec<String> = Vec::new();
            if !build.elapsed().is_zero() {
                parts.push(format!("built in {}", format_duration(build.elapsed())));
            }
            if let ImageProgress::Archived { size, .. } = &build.image {
                parts.push(format_bytes(*size));
            }
            let all_diags = collect_diagnostics(build);
            let n_warn = all_diags
                .iter()
                .filter(|d| d.diag.level == CargoDiagLevel::Warning)
                .count();
            let n_err = all_diags
                .iter()
                .filter(|d| d.diag.level == CargoDiagLevel::Error)
                .count();
            if n_err > 0 {
                parts.push(format!("{n_err} errors"));
            }
            if n_warn > 0 {
                parts.push(format!("{n_warn} warnings"));
            }
            if let ImageProgress::Archived { path, .. } = &build.image {
                parts.push(path.display().to_string());
            }
            ui.label(
                egui::RichText::new(parts.join("  ·  "))
                    .monospace()
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
        });
        return;
    }

    // Full stepper while running — taffy flexbox handles sizing so
    // nodes + connectors never overflow the card.
    card(ui, |ui| {
        ui.add_space(6.0);
        let current_order = build.phase.as_ref().map(|p| p.order()).unwrap_or(0);

        // The stepper is a row of (node, connector, node, …). Nodes
        // are fixed-width columns (circle + label); connectors grow to
        // fill remaining space. The connectors need to sit at the
        // vertical center of the circles, not the center of the whole
        // row. We achieve this with a fixed top margin on connectors
        // equal to half the circle height (14px for 28px circles).
        let circle_h: f32 = 28.0; // non-current circle size
        let connector_top = circle_h / 2.0;

        taffy_tui(ui, egui::Id::new(("stepper", build.id.0)))
            .reserve_available_width()
            .style(Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                align_items: Some(AlignItems::Start),
                size: Size {
                    width: percent(1.0),
                    height: auto(),
                },
                gap: Size {
                    width: length(10.0),
                    height: length(0.0),
                },
                ..Default::default()
            })
            .show(|tui| {
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
                    let circle_size = if current { 34.0 } else { 28.0 };

                    // Node: fixed to circle size. The label is painted
                    // centered below without affecting the layout box,
                    // so wide labels like "METADATA" can't grow the node.
                    tui.style(Style {
                        flex_grow: 0.0,
                        flex_shrink: 0.0,
                        size: Size {
                            width: length(circle_size),
                            height: length(circle_size + 22.0),
                        },
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        align_items: Some(AlignItems::Center),
                        ..Default::default()
                    })
                    .ui(|ui| {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(circle_size, circle_size),
                            egui::Sense::hover(),
                        );
                        ui.painter()
                            .circle_filled(rect.center(), circle_size / 2.0, circle_col);
                        ui.painter().circle_stroke(
                            rect.center(),
                            circle_size / 2.0,
                            egui::Stroke::new(ring_w, ring_col),
                        );
                        let glyph = if past { icon::CHECK } else { phase_icon(phase) };
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            glyph,
                            egui::FontId::proportional(circle_size * 0.55),
                            icon_col,
                        );
                        // Paint the label centered below the circle,
                        // outside the layout box so it doesn't widen the
                        // node.
                        let label_pos = egui::pos2(rect.center().x, rect.bottom() + 8.0);
                        ui.painter().text(
                            label_pos,
                            egui::Align2::CENTER_TOP,
                            phase.label(),
                            egui::FontId::monospace(10.0),
                            text_col,
                        );
                    });

                    // Connector line between nodes — grows to fill space.
                    // Top margin positions it at the circle's vertical center.
                    if i + 1 < PHASE_ORDER.len() {
                        let next_past = (order_i + 1) < current_order;
                        let next_current = (order_i + 1) == current_order;
                        let line_col = if next_past {
                            theme::INFO
                        } else if past && next_current {
                            theme::ACCENT
                        } else {
                            theme::BORDER_STRONG
                        };
                        tui.style(Style {
                            flex_grow: 1.0,
                            flex_shrink: 1.0,
                            flex_basis: length(14.0),
                            min_size: Size {
                                width: length(4.0),
                                height: auto(),
                            },
                            size: Size {
                                width: auto(),
                                height: length(2.0),
                            },
                            margin: Rect {
                                left: length(0.0),
                                right: length(0.0),
                                top: length(connector_top - 1.0),
                                bottom: length(0.0),
                            },
                            ..Default::default()
                        })
                        .add_with_background_ui(
                            |ui, container| {
                                ui.painter().rect_filled(
                                    container.full_container(),
                                    egui::CornerRadius::ZERO,
                                    line_col,
                                );
                            },
                            |_, _| {},
                        );
                    }
                }
            });
        ui.add_space(4.0);
        // Image status inline.
        ui.horizontal(|ui| {
            let (glyph, text, col) = match &build.image {
                ImageProgress::None => (
                    icon::HOURGLASS_MEDIUM,
                    "awaiting pack".to_string(),
                    theme::TEXT_DIM,
                ),
                ImageProgress::Assembled { size } => (
                    icon::PACKAGE,
                    format!("assembled · {}", format_bytes(*size)),
                    theme::ACCENT,
                ),
                ImageProgress::Archived { size, .. } => (
                    icon::PACKAGE,
                    format!("{} · {}", icon::CHECK, format_bytes(*size)),
                    theme::INFO,
                ),
            };
            ui.colored_label(col, glyph);
            ui.label(egui::RichText::new(text).monospace().size(11.0).color(col));
        });
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
                .filter(|c| {
                    matches!(
                        c.state,
                        CrateBuildState::Building
                            | CrateBuildState::Measuring
                            | CrateBuildState::Linking
                    )
                })
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
    let has_body = failed || !c.cargo_messages.is_empty() || has_ipc;
    let default_open = failed || building;

    let border = if failed {
        Some(theme::ERROR)
    } else if matches!(c.state, CrateBuildState::Building) {
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
        // Lock the row's width to the card's available width so the
        // body content (horizontal_wrapped chips, diagnostic lines)
        // wraps instead of growing the card wider.
        let row_w = ui.available_width();
        ui.set_min_width(row_w);
        ui.set_max_width(row_w);

        if has_body {
            let id = ui.make_persistent_id(("crate_row", build.id.0, c.name.as_str()));
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                default_open,
            );
            // Track state transitions to auto-open/close once rather
            // than forcing every frame. The user can freely toggle
            // after the automatic action.
            let prev_key = id.with("prev_state");
            let prev: Option<CrateBuildState> = ui.ctx().data_mut(|d| d.get_temp(prev_key));
            let cur = c.state;
            if let Some(prev) = prev {
                if prev != CrateBuildState::Building && cur == CrateBuildState::Building {
                    state.set_open(true);
                } else if prev == CrateBuildState::Building
                    && cur != CrateBuildState::Building
                    && !failed
                {
                    state.set_open(false);
                }
            }
            // Auto-open on failure regardless of previous state.
            if failed && prev.map_or(true, |p| p != CrateBuildState::Failed) {
                state.set_open(true);
            }
            ui.ctx().data_mut(|d| d.insert_temp(prev_key, cur));
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
                            ui.small(egui::RichText::new(chevron).color(theme::TEXT_DIM));
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
                        egui::Id::new(("crate_row_body", build.id.0, c.name.as_str())),
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
    // Right-aligned state info.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
    });
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
    let summary = &c.cargo_summary;
    let has_messages = !c.cargo_messages.is_empty();
    let has_ipc = !c.provides.is_empty() || !c.uses.is_empty();

    let building = c.state == CrateBuildState::Building;

    let w = ui.available_width();
    ui.set_max_width(w);

    ui.vertical(|ui| {
        // ── Dependencies ──────────────────────────────────────────
        let total_deps = summary.deps_fresh + summary.deps_compiled.len();
        if total_deps > 0 || building {
            ui.horizontal(|ui| {
                ui.colored_label(
                    theme::TEXT_DIM,
                    egui::RichText::new(icon::TREE_STRUCTURE).size(11.0),
                );
                let mut parts = Vec::new();
                if !summary.deps_compiled.is_empty() {
                    parts.push(format!("{} compiled", summary.deps_compiled.len()));
                }
                if summary.deps_fresh > 0 {
                    parts.push(format!("{} cached", summary.deps_fresh));
                }
                if total_deps > 0 {
                    ui.label(
                        egui::RichText::new(format!("{total_deps} deps ({})", parts.join(", ")))
                            .monospace()
                            .size(10.0)
                            .color(theme::TEXT_DIM),
                    );
                }
            });
            if !summary.deps_compiled.is_empty() {
                let names: Vec<&str> = summary.deps_compiled.iter().map(|s| s.as_str()).collect();
                chip_row(ui, &c.name, "deps", &names);
            }
            ui.add_space(4.0);
        }

        // ── Diagnostics ───────────────────────────────────────────
        for d in &summary.diagnostics {
            let col = match d.level {
                CargoDiagLevel::Error => theme::ERROR,
                CargoDiagLevel::Warning => theme::WARN,
                _ => theme::TEXT_SECONDARY,
            };
            for line in d.rendered.lines() {
                ui.add(
                    egui::Label::new(egui::RichText::new(line).monospace().size(11.0).color(col))
                        .wrap(),
                );
            }
            ui.add_space(2.0);
        }

        // ── Raw errors (non-JSON cargo failures) ──────────────────
        for text in &summary.raw_errors {
            for line in text.lines() {
                ui.label(
                    egui::RichText::new(line)
                        .monospace()
                        .size(11.0)
                        .color(theme::ERROR),
                );
            }
        }

        // ── Fallback for failed crates with no messages ───────────
        if !has_messages && c.state == CrateBuildState::Failed {
            if let Some(err) = &c.error {
                ui.colored_label(theme::ERROR, err);
            }
        }

        // ── Empty state ───────────────────────────────────────────
        if !has_messages && !has_ipc && c.state != CrateBuildState::Failed {
            ui.label(
                egui::RichText::new("No build data")
                    .monospace()
                    .size(10.0)
                    .color(theme::TEXT_DIM),
            );
        }

        // ── IPC metadata ──────────────────────────────────────────
        if has_ipc {
            if has_messages {
                ui.add_space(4.0);
            }
            if !c.provides.is_empty() {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        theme::TEXT_DIM,
                        egui::RichText::new(icon::PLUGS_CONNECTED).size(11.0),
                    );
                    ui.label(
                        egui::RichText::new(format!("provides {} resources", c.provides.len()))
                            .monospace()
                            .size(10.0)
                            .color(theme::TEXT_DIM),
                    );
                });
                let names: Vec<&str> = c.provides.iter().map(|p| p.resource.as_str()).collect();
                chip_row(ui, &c.name, "provides", &names);
            }
            if !c.uses.is_empty() {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.colored_label(
                        theme::TEXT_DIM,
                        egui::RichText::new(icon::ARROW_SQUARE_OUT).size(11.0),
                    );
                    ui.label(
                        egui::RichText::new(format!("uses {} resources", c.uses.len()))
                            .monospace()
                            .size(10.0)
                            .color(theme::TEXT_DIM),
                    );
                });
                let names: Vec<String> = c
                    .uses
                    .iter()
                    .map(|u| {
                        if u.resource.is_empty() {
                            u.server_task.clone()
                        } else {
                            format!("{}::{}", u.server_task, u.resource)
                        }
                    })
                    .collect();
                let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                chip_row(ui, &c.name, "uses", &name_refs);
            }
        }
    });
}

/// Chip variant. `Pill` matches provided resources (accent-filled,
/// soft pill). `Boxy` matches consumed resources (monospace code
/// identifier in a bordered box).
/// Taffy-based wrapping chip row. Used for deps, provides, and uses.
fn chip_row(ui: &mut egui::Ui, crate_name: &str, kind: &str, names: &[&str]) {
    taffy_tui(ui, ui.id().with(("chips", crate_name, kind)))
        .reserve_available_width()
        .style(Style {
            display: Display::Flex,
            flex_wrap: FlexWrap::Wrap,
            flex_direction: FlexDirection::Row,
            gap: Size {
                width: length(4.0),
                height: length(3.0),
            },
            padding: Rect {
                left: length(18.0),
                right: length(0.0),
                top: length(0.0),
                bottom: length(0.0),
            },
            size: Size {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        })
        .show(|tui| {
            for name in names {
                tui.style(Style::default()).add_with_background_ui(
                    |ui, container| {
                        ui.painter().rect_filled(
                            container.full_container(),
                            egui::CornerRadius::same(3),
                            theme::BG,
                        );
                    },
                    |tui, _| {
                        tui.style(Style {
                            padding: Rect {
                                left: length(5.0),
                                right: length(5.0),
                                top: length(1.0),
                                bottom: length(1.0),
                            },
                            ..Default::default()
                        })
                        .ui(|ui| {
                            ui.label(
                                egui::RichText::new(*name)
                                    .monospace()
                                    .size(10.0)
                                    .color(theme::TEXT_SECONDARY),
                            );
                        });
                    },
                );
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
            ui.colored_label(theme::TEXT_DIM, format!("{} devices", build.memories.len()));
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
fn memory_device_row(ui: &mut egui::Ui, build: &BuildHandle, dev: &crate::state::MemoryDevice) {
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
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
        });
    });

    let bar_w = ui.available_width();
    // Allocate the bar as non-interactive — hover is handled manually.
    let (track_rect, _) =
        ui.allocate_exact_size(egui::vec2(bar_w, 8.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(track_rect, egui::CornerRadius::ZERO, theme::BG);

    if capacity == 0 {
        return;
    }

    // Build segment rects first, then draw + hover.
    struct Seg {
        rect: egui::Rect,
        color: egui::Color32,
        idx: usize,
    }
    let gap = 1.0;
    let n = allocs.len();
    let mut segs = Vec::with_capacity(n);
    let mut x = track_rect.left();
    for (i, a) in allocs.iter().enumerate() {
        let w = ((a.size as f32 / capacity as f32) * track_rect.width()).max(1.0);
        let right = if i + 1 < n {
            (x + w).min(track_rect.right())
        } else {
            (x + w).min(track_rect.right())
        };
        let rect = egui::Rect::from_min_max(
            egui::pos2(x, track_rect.top()),
            egui::pos2(right, track_rect.bottom()),
        );
        segs.push(Seg {
            rect,
            color: alloc_color(&a.owner),
            idx: i,
        });
        x = right + gap;
    }

    // Draw segments.
    for seg in &segs {
        ui.painter()
            .rect_filled(seg.rect, egui::CornerRadius::ZERO, seg.color);
    }

    // Hover: check pointer position against segments.
    if let Some(pointer) = ui.ctx().pointer_hover_pos() {
        if track_rect.contains(pointer) {
            for seg in &segs {
                if seg.rect.contains(pointer) {
                    let a = &allocs[seg.idx];
                    egui::popup::show_tooltip_at(
                        ui.ctx(),
                        ui.layer_id(),
                        ui.id().with(("alloc_tip", seg.idx)),
                        pointer + egui::vec2(12.0, 12.0),
                        |ui| {
                            ui.label(
                                egui::RichText::new(format!("{}.{}", a.owner, a.region))
                                    .monospace()
                                    .strong()
                                    .color(seg.color),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} @ {:#010x}",
                                    format_bytes(a.size),
                                    a.base
                                ))
                                .monospace()
                                .size(11.0)
                                .color(theme::TEXT_SECONDARY),
                            );
                        },
                    );
                    break;
                }
            }
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
                    egui::RichText::new(format!("·  {} mappings", dev.mappings.len()))
                        .monospace()
                        .color(theme::TEXT_DIM),
                );
            }
        });
    }
}

/// Deterministic color for an allocation owner. Known roles get
/// fixed colors; task crates get a hue derived from their name.
fn alloc_color(owner: &str) -> egui::Color32 {
    match owner {
        "kernel" => theme::WARN,
        "bootloader" => theme::ACCENT,
        _ => {
            // Hash the name to pick a hue.
            let mut h: u32 = 0;
            for b in owner.bytes() {
                h = h.wrapping_mul(31).wrapping_add(b as u32);
            }
            let hue = (h % 360) as f32;
            egui::ecolor::Hsva::new(hue / 360.0, 0.6, 0.7, 1.0).into()
        }
    }
}

// ── Footer ──────────────────────────────────────────────────────────────

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
    let all = collect_diagnostics(build);
    let warnings: Vec<_> = all
        .iter()
        .filter(|d| d.diag.level == CargoDiagLevel::Warning)
        .collect();
    let errors: Vec<_> = all
        .iter()
        .filter(|d| d.diag.level == CargoDiagLevel::Error)
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                pill(ui, theme::ERROR, &format!("{} ERRORS", errors.len()));
                pill(ui, theme::WARN, &format!("{} WARNINGS", warnings.len()));
            });
        });
        ui.add_space(6.0);
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

fn diagnostic_row(ui: &mut egui::Ui, d: &BuildDiagnostic<'_>) {
    let diag = &d.diag;
    let (stripe, level_text) = match diag.level {
        CargoDiagLevel::Error => (theme::ERROR, "error"),
        CargoDiagLevel::Warning => (theme::WARN, "warning"),
        CargoDiagLevel::Note => (theme::ACCENT, "note"),
        CargoDiagLevel::Help => (theme::INFO, "help"),
    };
    egui::Frame::NONE
        .fill(theme::BG)
        .corner_radius(egui::CornerRadius::same(6))
        .stroke(egui::Stroke::new(1.0, stripe.gamma_multiply(0.25)))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(3.0, 34.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, egui::CornerRadius::same(2), stripe);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            stripe,
                            egui::RichText::new(level_text)
                                .monospace()
                                .strong()
                                .size(11.0),
                        );
                        if let Some(code) = &diag.code {
                            ui.label(
                                egui::RichText::new(format!("[{code}]"))
                                    .monospace()
                                    .size(10.0)
                                    .color(stripe.gamma_multiply(0.7)),
                            );
                        }
                        ui.colored_label(
                            theme::TEXT_SECONDARY,
                            egui::RichText::new(d.crate_name).monospace().size(11.0),
                        );
                        if let Some(loc) = &diag.location {
                            ui.label(
                                egui::RichText::new(loc)
                                    .monospace()
                                    .size(10.0)
                                    .color(theme::TEXT_DIM),
                            );
                        }
                    });
                    ui.label(
                        egui::RichText::new(&diag.message)
                            .monospace()
                            .size(12.0)
                            .color(theme::TEXT_PRIMARY),
                    );
                });
            });
        });
    ui.add_space(4.0);
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn card(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    taffy_tui(ui, ui.auto_id_with("card"))
        .reserve_available_width()
        .style(Style {
            size: Size {
                width: percent(1.0),
                height: auto(),
            },
            ..Default::default()
        })
        .show(|tui| {
            tui.style(Style {
                size: Size {
                    width: percent(1.0),
                    height: auto(),
                },
                padding: Rect {
                    left: length(18.0),
                    right: length(18.0),
                    top: length(14.0),
                    bottom: length(14.0),
                },
                ..Default::default()
            })
            .add_with_background_ui(
                |ui, container| {
                    let rect = container.full_container();
                    ui.painter()
                        .rect_filled(rect, egui::CornerRadius::same(8), theme::SURFACE);
                    ui.painter().rect_stroke(
                        rect,
                        egui::CornerRadius::same(8),
                        egui::Stroke::new(1.0, theme::SURFACE_BORDER),
                        egui::StrokeKind::Inside,
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
                    .ui(|ui| content(ui));
                },
            );
        });
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

// ── Diagnostics helpers ───────────────────────────────────────────────

struct BuildDiagnostic<'a> {
    crate_name: &'a str,
    diag: &'a CargoDiagnostic,
}

fn collect_diagnostics(build: &BuildHandle) -> Vec<BuildDiagnostic<'_>> {
    let mut out = Vec::new();
    for c in &build.crates {
        for d in &c.cargo_summary.diagnostics {
            if matches!(d.level, CargoDiagLevel::Error | CargoDiagLevel::Warning) {
                out.push(BuildDiagnostic {
                    crate_name: &c.name,
                    diag: d,
                });
            }
        }
    }
    out
}
