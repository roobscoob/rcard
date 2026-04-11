use crate::theme;
use egui_phosphor::regular as icon;

pub fn show(ui: &mut egui::Ui, name: &str) {
    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 2.0 - 30.0);
            ui.colored_label(
                theme::TEXT_DIM,
                egui::RichText::new(icon::WRENCH).size(32.0),
            );
            ui.colored_label(theme::TEXT_SECONDARY, format!("{name} — coming soon"));
        });
    });
}
