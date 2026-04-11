use crate::state::{SerialAdapterType, SerialPortConfig, SerialPortStatus};
use crate::theme;
use egui_phosphor::regular as icon;

pub enum SerialAdapterAction {
    None,
    Detach,
}

pub fn show(ui: &mut egui::Ui, cfg: &SerialPortConfig) -> SerialAdapterAction {
    let mut action = SerialAdapterAction::None;

    let type_label = match cfg.adapter_type {
        SerialAdapterType::Usart1 => "USART1",
        SerialAdapterType::Usart2 => "USART2",
    };

    let (status_color, status_text, status_icon) = match cfg.status {
        SerialPortStatus::Connecting => (theme::TEXT_DIM, "connecting...", icon::CIRCLE_NOTCH),
        SerialPortStatus::PortOpen => (theme::WARN, "port open", icon::CIRCLE_HALF),
        SerialPortStatus::DeviceDetected => (theme::INFO, "connected", icon::CHECK_CIRCLE),
        SerialPortStatus::Error => (theme::ERROR, "error", icon::WARNING_CIRCLE),
    };

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.heading(format!("{} {}", icon::PLUG, &cfg.port));
    });
    ui.add_space(8.0);

    egui::Frame::new()
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(theme::TEXT_SECONDARY, format!("{} Type:", icon::TAG));
                ui.strong(type_label);
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.colored_label(theme::TEXT_SECONDARY, format!("{} Status:", icon::INFO));
                ui.colored_label(status_color, status_icon);
                ui.colored_label(status_color, status_text);
            });

            ui.add_space(16.0);
            ui.separator();
            ui.add_space(8.0);

            if ui
                .button(format!("{} Detach", icon::X))
                .clicked()
            {
                action = SerialAdapterAction::Detach;
            }
        });

    action
}