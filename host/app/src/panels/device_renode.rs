use crate::state::DeviceHandle;
use crate::theme;
use egui_phosphor::regular as icon;

pub enum RenodeAction {
    None,
    Kill,
}

pub fn show(ui: &mut egui::Ui, dev: &DeviceHandle) -> RenodeAction {
    let mut action = RenodeAction::None;

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.heading(format!("{} Renode — {}", icon::DESKTOP, dev.name));
    });
    ui.add_space(8.0);

    egui::Frame::new()
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.colored_label(
                theme::TEXT_SECONDARY,
                "Renode emulator is running.",
            );
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            if ui
                .button(format!("{} Kill Emulator", icon::X_CIRCLE))
                .clicked()
            {
                action = RenodeAction::Kill;
            }
        });

    action
}
