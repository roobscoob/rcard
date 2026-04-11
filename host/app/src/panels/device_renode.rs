use crate::state::DeviceHandle;
use crate::theme;
use egui_phosphor::regular as icon;
use egui_taffy::taffy::prelude::*;
use egui_taffy::{tui as taffy_tui, TuiBuilderLogic};

pub enum RenodeAction {
    None,
    Kill,
}

pub fn show(ui: &mut egui::Ui, dev: &DeviceHandle) -> RenodeAction {
    let mut action = RenodeAction::None;

    taffy_tui(ui, egui::Id::new(("device_renode_root", dev.name.as_str())))
        .reserve_available_space()
        .style(Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            align_items: Some(AlignItems::Stretch),
            size: Size { width: percent(1.0), height: percent(1.0) },
            gap: length(8.0),
            padding: length(12.0),
            ..Default::default()
        })
        .show(|tui| {
            tui.heading(format!("{} Renode — {}", icon::DESKTOP, dev.name));
            tui.colored_label(theme::TEXT_SECONDARY, "Renode emulator is running.");
            tui.separator();
            if tui
                .ui_add(egui::Button::new(format!("{} Kill Emulator", icon::X_CIRCLE)))
                .clicked()
            {
                action = RenodeAction::Kill;
            }
        });

    action
}
