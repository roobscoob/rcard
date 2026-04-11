use std::collections::VecDeque;

use egui::{Color32, FontFamily, FontId, Margin, RichText, ScrollArea, Stroke, TextStyle};

const BG: Color32 = Color32::from_rgb(0x0c, 0x0e, 0x14);
const BORDER: Color32 = Color32::from_rgb(0x2a, 0x2f, 0x3d);
const TEXT: Color32 = Color32::from_rgb(0xc8, 0xd0, 0xe0);
const TEXT_DIM: Color32 = Color32::from_rgb(0x55, 0x5d, 0x73);
const FONT_SIZE: f32 = 12.5;
const LINE_HEIGHT: f32 = 16.0;

/// Render a terminal-style scrollback view of raw text lines.
///
/// Expects to be called inside a bounded parent (e.g. a taffy `flex: 1`
/// node) — the ScrollArea fills the parent's available rect.
pub fn show(ui: &mut egui::Ui, lines: &VecDeque<String>) {
    let frame = egui::Frame::new()
        .fill(BG)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(6.0)
        .inner_margin(Margin::symmetric(10, 8));

    frame.show(ui, |ui| {
        if lines.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("(no output yet)")
                        .color(TEXT_DIM)
                        .family(FontFamily::Monospace)
                        .size(FONT_SIZE),
                );
            });
            return;
        }

        let mut style = (*ui.ctx().style()).clone();
        style.text_styles.insert(
            TextStyle::Body,
            FontId::new(FONT_SIZE, FontFamily::Monospace),
        );
        style.visuals.override_text_color = Some(TEXT);
        ui.set_style(style);

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show_rows(ui, LINE_HEIGHT, lines.len(), |ui, row_range| {
                for i in row_range {
                    let line = &lines[i];
                    if line.is_empty() {
                        ui.allocate_exact_size(
                            egui::vec2(0.0, LINE_HEIGHT),
                            egui::Sense::hover(),
                        );
                    } else {
                        ui.label(line);
                    }
                }
            });
    });
}
