use device::logs::LogEntry;
use egui_extras::{Column, TableBuilder};
use rcard_log::LogLevel;

use crate::theme;
use egui_phosphor::regular as icon;

const ROW_HEIGHT: f32 = 20.0;

/// Render decoded structured log entries with no metadata lookups.
///
/// Used by the USART2 serial adapter management panel: we don't have a
/// firmware metadata table at the port level, so species and values are
/// shown in their raw decoded form (hex species hash, `Debug`-formatted
/// values). No adapter column — the panel already names the adapter.
pub fn show(ui: &mut egui::Ui, entries: &[LogEntry]) {
    if entries.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 2.0 - 30.0);
                ui.colored_label(theme::TEXT_DIM, egui::RichText::new(icon::SCROLL).size(32.0));
                ui.colored_label(theme::TEXT_DIM, "Waiting for logs...");
            });
        });
        return;
    }

    let table = TableBuilder::new(ui)
        .striped(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .stick_to_bottom(true)
        .column(Column::exact(70.0))   // level
        .column(Column::exact(60.0))   // source (task index)
        .column(Column::exact(160.0))  // species (hex)
        .column(Column::remainder());  // values

    table
        .header(ROW_HEIGHT, |mut header| {
            header.col(|ui| {
                ui.colored_label(theme::TEXT_SECONDARY, "Level");
            });
            header.col(|ui| {
                ui.colored_label(theme::TEXT_SECONDARY, format!("{} Task", icon::GEAR));
            });
            header.col(|ui| {
                ui.colored_label(theme::TEXT_SECONDARY, "Species");
            });
            header.col(|ui| {
                ui.colored_label(theme::TEXT_SECONDARY, "Values");
            });
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, entries.len(), |mut row| {
                let entry = &entries[row.index()];

                let (color, label) = level_style(entry.level);
                row.col(|ui| {
                    ui.colored_label(color, label);
                });
                row.col(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(0x80, 0x87, 0xa2),
                        format!("t{}", entry.source),
                    );
                });
                row.col(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(0x80, 0x87, 0xa2),
                        format!("{:016x}", entry.log_species),
                    );
                });
                row.col(|ui| {
                    let txt: String = entry
                        .values
                        .iter()
                        .map(|v| format!("{v:?}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    ui.label(txt);
                });
            });
        });
}

fn level_style(level: LogLevel) -> (egui::Color32, String) {
    match level {
        LogLevel::Panic => (theme::ERROR, format!("{} PNC", icon::SKULL)),
        LogLevel::Error => (theme::ERROR, format!("{} ERR", icon::X_CIRCLE)),
        LogLevel::Warn => (theme::WARN, format!("{} WRN", icon::WARNING)),
        LogLevel::Info => (theme::INFO, format!("{} INF", icon::INFO)),
        LogLevel::Debug => (theme::DEBUG, format!("{} DBG", icon::BUG)),
        LogLevel::Trace => (theme::TRACE, format!("{} TRC", icon::DOT)),
    }
}
