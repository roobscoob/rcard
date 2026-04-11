use crate::state::{BuildHandle, BuildStatus};
use crate::theme;
use egui_phosphor::regular as icon;
use egui_taffy::taffy::prelude::*;
use egui_taffy::{tui as taffy_tui, TuiBuilderLogic};

pub fn show(ui: &mut egui::Ui, build: &BuildHandle) {
    taffy_tui(ui, egui::Id::new(("build_output_root", build.id.0)))
        .reserve_available_space()
        .style(Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            align_items: Some(AlignItems::Stretch),
            size: Size { width: percent(1.0), height: percent(1.0) },
            gap: length(4.0),
            ..Default::default()
        })
        .show(|tui| {
            // Status header.
            tui.style(Style {
                flex_shrink: 0.0,
                ..Default::default()
            })
            .ui(|ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let (color, status_icon, label) = match &build.status {
                        BuildStatus::Running { stage, detail } => {
                            let text = if detail.is_empty() {
                                stage.clone()
                            } else {
                                format!("{stage}: {detail}")
                            };
                            (theme::WARN, icon::CIRCLE_NOTCH, text)
                        }
                        BuildStatus::Succeeded { .. } => {
                            (theme::INFO, icon::CHECK_CIRCLE, "Succeeded".into())
                        }
                        BuildStatus::Failed { error } => {
                            (theme::ERROR, icon::X_CIRCLE, format!("Failed: {error}"))
                        }
                    };
                    ui.colored_label(color, status_icon);
                    ui.colored_label(color, &label);
                });
                ui.separator();
            });

            // Log body fills remaining space.
            tui.style(Style {
                flex_grow: 1.0,
                min_size: Size { width: auto(), height: length(0.0) },
                ..Default::default()
            })
            .ui(|ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.style_mut().override_text_style = Some(egui::TextStyle::Monospace);
                        for line in &build.log {
                            ui.label(line);
                        }
                    });
            });
        });
}
