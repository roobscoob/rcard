use crate::panels::{raw_logs, raw_terminal, serial_control};
use crate::state::{SerialAdapterType, SerialPortConfig, SerialPortStatus};
use crate::theme;
use egui_phosphor::regular as icon;
use egui_taffy::taffy::prelude::*;
use egui_taffy::{tui, TuiBuilderLogic};

pub enum SerialAdapterAction {
    None,
    Detach,
}

/// Render the USART1 adapter pane: header + raw terminal body.
pub fn show(ui: &mut egui::Ui, cfg: &SerialPortConfig) -> SerialAdapterAction {
    render(ui, cfg, "usart1_pane_root", |ui| {
        raw_terminal::show(ui, &cfg.raw_lines);
    })
}

/// Render the USART2 Logs sub-pane: header + decoded structured logs.
pub fn show_logs(ui: &mut egui::Ui, cfg: &SerialPortConfig) -> SerialAdapterAction {
    render(ui, cfg, "usart2_logs_pane_root", |ui| {
        raw_logs::show(ui, &cfg.structured_logs);
    })
}

/// Render the USART2 Control sub-pane: header + decoded IPC events.
pub fn show_control(ui: &mut egui::Ui, cfg: &SerialPortConfig) -> SerialAdapterAction {
    render(ui, cfg, "usart2_control_pane_root", |ui| {
        serial_control::show(ui, &cfg.control_events);
    })
}

fn render(
    ui: &mut egui::Ui,
    cfg: &SerialPortConfig,
    id_salt: &'static str,
    body: impl FnOnce(&mut egui::Ui),
) -> SerialAdapterAction {
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

    tui(ui, egui::Id::new((id_salt, cfg.port.as_str())))
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
            // Header row.
            tui.style(Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                align_items: Some(AlignItems::Center),
                padding: length(6.0),
                gap: length(8.0),
                flex_shrink: 0.0,
                ..Default::default()
            })
            .add(|tui| {
                tui.heading(format!("{} {}", icon::PLUG, &cfg.port));
                tui.separator();
                tui.label(
                    egui::RichText::new(type_label).color(theme::TEXT_SECONDARY),
                );
                tui.separator();
                tui.label(egui::RichText::new(status_icon).color(status_color));
                tui.label(egui::RichText::new(status_text).color(status_color));

                // Spacer pushes the detach button to the right edge.
                tui.style(Style {
                    flex_grow: 1.0,
                    ..Default::default()
                })
                .add_empty();

                if tui
                    .ui_add(egui::Button::new(format!("{} Detach", icon::X)))
                    .clicked()
                {
                    action = SerialAdapterAction::Detach;
                }
            });

            tui.separator();

            // Body — fills remaining space.
            tui.style(Style {
                flex_grow: 1.0,
                padding: length(6.0),
                min_size: Size { width: auto(), height: length(0.0) },
                ..Default::default()
            })
            .ui(body);
        });

    action
}
