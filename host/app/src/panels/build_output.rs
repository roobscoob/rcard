use crate::state::{BuildHandle, BuildStatus};
use crate::theme;
use egui_phosphor::regular as icon;

pub fn show(ui: &mut egui::Ui, build: &BuildHandle) {
    ui.add_space(4.0);

    // Status header.
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
            BuildStatus::Succeeded { .. } => (theme::INFO, icon::CHECK_CIRCLE, "Succeeded".into()),
            BuildStatus::Failed { error } => (theme::ERROR, icon::X_CIRCLE, format!("Failed: {error}")),
        };
        ui.colored_label(color, status_icon);
        ui.colored_label(color, &label);
    });

    ui.separator();

    // Build log.
    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.style_mut().override_text_style = Some(egui::TextStyle::Monospace);
            for line in &build.log {
                ui.label(line);
            }
        });
}
