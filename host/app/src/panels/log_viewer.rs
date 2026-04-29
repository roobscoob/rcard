use std::collections::HashMap;

use device::logs::LogContents;
use egui_extras::{Column, TableBuilder};
use rcard_log::{LogLevel, OwnedValue};

use crate::state::{AppState, DeviceHandle};
use crate::theme;
use egui_phosphor::regular as icon;
use tfw::archive::TfwMetadata;

const ROW_HEIGHT: f32 = 20.0;

pub fn show(ui: &mut egui::Ui, dev: &DeviceHandle, state: &AppState) {
    let total_rows = dev.log_buffer.len();

    if total_rows == 0 {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 2.0 - 30.0);
                ui.colored_label(theme::TEXT_DIM, egui::RichText::new(icon::SCROLL).size(32.0));
                ui.colored_label(theme::TEXT_DIM, "Waiting for logs...");
            });
        });
        return;
    }

    // Firmware metadata for resolving task names, species, types.
    let metadata: Option<&TfwMetadata> = dev
        .firmware_id
        .and_then(|fw_id| state.firmware.get(&fw_id))
        .map(|fw| &fw.metadata);

    let table = TableBuilder::new(ui)
        .striped(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .stick_to_bottom(true)
        .column(Column::exact(70.0))  // adapter
        .column(Column::exact(80.0))  // tick
        .column(Column::exact(80.0))  // source
        .column(Column::exact(70.0))  // level
        .column(Column::remainder()); // message

    table
        .header(ROW_HEIGHT, |mut header| {
            header.col(|ui| { ui.colored_label(theme::TEXT_SECONDARY, format!("{} Adapter", icon::PLUG)); });
            header.col(|ui| { ui.colored_label(theme::TEXT_SECONDARY, format!("{} Tick", icon::CLOCK)); });
            header.col(|ui| { ui.colored_label(theme::TEXT_SECONDARY, format!("{} Source", icon::GEAR)); });
            header.col(|ui| { ui.colored_label(theme::TEXT_SECONDARY, "Level"); });
            header.col(|ui| { ui.colored_label(theme::TEXT_SECONDARY, "Message"); });
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, total_rows, |mut row| {
                let idx = row.index();
                let log = &dev.log_buffer[idx];

                // Adapter column.
                row.col(|ui| {
                    let names: Vec<&str> = log.adapters.iter()
                        .map(|id| state.adapters
                            .get(id)
                            .map(|a| a.display_name.as_str())
                            .unwrap_or("?"))
                        .collect();
                    ui.colored_label(
                        egui::Color32::from_rgb(0x60, 0x65, 0x80),
                        names.join(", "),
                    );
                });

                // Tick column.
                row.col(|ui| {
                    if let Some(tick) = log.device_tick {
                        ui.colored_label(
                            egui::Color32::from_rgb(0x60, 0x65, 0x80),
                            format!("{tick}"),
                        );
                    }
                });

                match &log.contents {
                    LogContents::Structured(entry) => {
                        // Source column. sysmodule_* tasks are core services —
                        // strip the prefix and paint them blue; everything else
                        // is application code, painted green.
                        row.col(|ui| {
                            let source = metadata
                                .and_then(|m| m.task_names.get(entry.source as usize))
                                .map(|s| s.as_str())
                                .unwrap_or("?");
                            let (display, color) = match source.strip_prefix("sysmodule_") {
                                Some(rest) => (rest, theme::ACCENT),
                                None => (source, theme::INFO),
                            };
                            ui.colored_label(color, display);
                        });
                        let (color, label) = level_style(entry.level);
                        row.col(|ui| { ui.colored_label(color, label); });
                        row.col(|ui| {
                            let msg = if let Some(meta) = metadata {
                                if let Some(species) = meta.species.get(&entry.log_species) {
                                    format_species(&species.format, &entry.values, &meta.type_names)
                                } else {
                                    format_values_fallback(&entry.values, entry.log_species)
                                }
                            } else {
                                format_values_fallback(&entry.values, entry.log_species)
                            };
                            ui.label(&msg);
                        });
                    }
                    LogContents::Text(text) => {
                        // Parse "source: message" prefix.
                        let (source, message) = text
                            .find(": ")
                            .filter(|&i| i > 0 && i < 20 && text[..i].chars().all(|c| c.is_alphanumeric() || c == '_'))
                            .map(|i| (&text[..i], &text[i + 2..]))
                            .unwrap_or(("", text.as_str()));
                        row.col(|ui| {
                            if !source.is_empty() {
                                ui.colored_label(
                                    egui::Color32::from_rgb(0x80, 0x87, 0xA2),
                                    source,
                                );
                            }
                        });
                        row.col(|_ui| {}); // level
                        row.col(|ui| { ui.label(message); });
                    }
                    LogContents::Auxiliary { name, text } => {
                        row.col(|_ui| {}); // source
                        row.col(|ui| {
                            ui.colored_label(
                                egui::Color32::from_rgb(0x80, 0x87, 0xA2),
                                format!("[{name}]"),
                            );
                        });
                        row.col(|ui| { ui.label(text); });
                    }
                    LogContents::Renode { level, message } => {
                        let (source, body) = message
                            .find(": ")
                            .filter(|&i| i > 0 && i < 20 && message[..i].chars().all(|c| c.is_alphanumeric() || c == '_'))
                            .map(|i| (&message[..i], &message[i + 2..]))
                            .unwrap_or(("", message.as_str()));
                        row.col(|ui| {
                            if !source.is_empty() {
                                ui.colored_label(
                                    egui::Color32::from_rgb(0x80, 0x87, 0xA2),
                                    source,
                                );
                            }
                        });
                        let (color, label) = level_style(*level);
                        row.col(|ui| { ui.colored_label(color, label); });
                        row.col(|ui| { ui.label(body); });
                    }
                }
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

// ── Species / value formatting ─────────────────────────────────────────

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

fn format_values_fallback(values: &[OwnedValue], species: u64) -> String {
    if values.is_empty() {
        format!("species=0x{species:x}")
    } else {
        let vals: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
        format!("{} (species=0x{species:x})", vals.join(", "))
    }
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
                if inner.is_empty() { name.clone() } else { format!("{name}({})", inner.join(", ")) }
            } else {
                format!("({})", inner.join(", "))
            }
        }
        Struct { type_id, fields } => {
            let name = type_names.get(type_id);
            if fields.is_empty() {
                name.cloned().unwrap_or_else(|| "{}".into())
            } else {
                let inner: Vec<String> = fields.iter().map(|(_, v)| format_value(v, type_names)).collect();
                if let Some(name) = name {
                    format!("{name} {{{}}}", inner.join(", "))
                } else {
                    format!("{{{}}}", inner.join(", "))
                }
            }
        }
        StackDump { sp, stack, .. } => {
            format!("<stack dump: sp=0x{sp:08x}, {} bytes>", stack.len())
        }
    }
}
