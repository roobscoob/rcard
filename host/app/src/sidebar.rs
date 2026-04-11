use crate::panels::Pane;
use crate::state::*;
use crate::theme;
use egui_phosphor::regular as icon;
use egui_ltreeview::{Action, TreeView};
use egui_taffy::taffy::prelude::*;
use egui_taffy::{tui as taffy_tui, TuiBuilderLogic};

/// Render the sidebar content based on the active section.
pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    match state.sidebar_section {
        SidebarSection::Firmware => firmware_panel(ui, state),
        SidebarSection::Adapters => adapters_panel(ui, state),
        SidebarSection::Devices => devices_panel(ui, state),
    }
}

// ── Firmware ────────────────────────────────────────────────────────────

fn firmware_panel(ui: &mut egui::Ui, state: &mut AppState) {
    let mut tree = std::mem::replace(
        &mut state.firmware_sidebar_tree,
        egui_tiles::Tree::empty("temp"),
    );
    let mut behavior = FirmwareSidebarBehavior { state };
    tree.ui(&mut behavior, ui);
    behavior.state.firmware_sidebar_tree = tree;
}

struct FirmwareSidebarBehavior<'a> {
    state: &'a mut AppState,
}

impl<'a> egui_tiles::Behavior<SidebarPane> for FirmwareSidebarBehavior<'a> {
    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        _tile_id: egui_tiles::TileId,
        pane: &mut SidebarPane,
    ) -> egui_tiles::UiResponse {
        match pane {
            SidebarPane::Build => build_section(ui, self.state),
            SidebarPane::FirmwareList => firmware_list_section(ui, self.state),
        }
        egui_tiles::UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &SidebarPane) -> egui::WidgetText {
        match pane {
            SidebarPane::Build => format!("{} Build", icon::HAMMER).into(),
            SidebarPane::FirmwareList => format!("{} Firmware", icon::PACKAGE).into(),
        }
    }

    fn is_tab_closable(&self, _tiles: &egui_tiles::Tiles<SidebarPane>, _tile_id: egui_tiles::TileId) -> bool {
        false
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        egui_tiles::SimplificationOptions {
            all_panes_must_have_tabs: false,
            ..Default::default()
        }
    }
}

fn build_section(ui: &mut egui::Ui, state: &mut AppState) {
    // Firmware directory — path display + browse button.
    ui.colored_label(theme::TEXT_SECONDARY, format!("{} Source Directory", icon::FOLDER));
    ui.add_space(2.0);

    if state.firmware_dir_input.is_empty() {
        ui.colored_label(theme::TEXT_DIM, "No directory set");
    } else {
        ui.colored_label(theme::TEXT_SECONDARY, &state.firmware_dir_input);
    }

    if ui.button(format!("{} Browse...", icon::FOLDER_OPEN)).clicked() {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Select firmware directory")
            .pick_folder()
        {
            state.firmware_dir_input = path.display().to_string();
            state.refresh_build_options();
        }
    }

    ui.add_space(8.0);

    if state.firmware_dir_input.is_empty() || state.build_configs.is_empty() {
        if !state.firmware_dir_input.is_empty() {
            ui.colored_label(theme::TEXT_DIM, "No .ncl configs found");
        }
        return;
    }

    ui.separator();
    ui.add_space(4.0);

    // Config picker.
    ui.horizontal(|ui| {
        ui.colored_label(theme::TEXT_SECONDARY, format!("{}", icon::GEAR_SIX));
        ui.label("Config");
        egui::ComboBox::from_id_salt("build_config")
            .width(100.0)
            .show_index(
                ui,
                &mut state.selected_config,
                state.build_configs.len(),
                |i| state.build_configs.get(i).cloned().unwrap_or_default(),
            );
    });

    // Board picker.
    if !state.build_boards.is_empty() {
        ui.horizontal(|ui| {
            ui.colored_label(theme::TEXT_SECONDARY, format!("{}", icon::CPU));
            ui.label("Board");
            egui::ComboBox::from_id_salt("build_board")
                .width(100.0)
                .show_index(
                    ui,
                    &mut state.selected_board,
                    state.build_boards.len(),
                    |i| state.build_boards.get(i).cloned().unwrap_or_default(),
                );
        });
    }

    // Layout picker.
    if !state.build_layouts.is_empty() {
        ui.horizontal(|ui| {
            ui.colored_label(theme::TEXT_SECONDARY, format!("{}", icon::LAYOUT));
            ui.label("Layout");
            egui::ComboBox::from_id_salt("build_layout")
                .width(100.0)
                .show_index(
                    ui,
                    &mut state.selected_layout,
                    state.build_layouts.len(),
                    |i| state.build_layouts.get(i).cloned().unwrap_or_default(),
                );
        });
    }

    ui.add_space(6.0);

    let any_running = state.builds.values().any(|b| {
        matches!(b.status, BuildStatus::Running { .. })
    });

    let label = if any_running {
        format!("{} Building...", icon::CIRCLE_NOTCH)
    } else {
        format!("{} Build", icon::HAMMER)
    };
    let button = egui::Button::new(label);
    if ui.add_enabled(!any_running, button).clicked() {
        state.start_build();
    }
}

fn firmware_list_section(ui: &mut egui::Ui, state: &mut AppState) {
    if state.firmware.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            ui.colored_label(theme::TEXT_DIM, format!("{}", icon::DOWNLOAD_SIMPLE));
            ui.colored_label(
                theme::TEXT_DIM,
                "Drop a .tfw file or\nbuild one above",
            );
        });
        return;
    }

    let mut entries: Vec<(FirmwareId, String)> = state
        .firmware
        .iter()
        .map(|(id, fw)| (*id, fw.display_name()))
        .collect();
    entries.sort_by(|a, b| a.1.cmp(&b.1));

    let tree_id = ui.make_persistent_id("firmware_tree");
    let (_response, actions) = TreeView::new(tree_id)
        .allow_multi_selection(false)
        .fill_space_horizontal(true)
        .show(ui, |builder| {
            for (id, name) in &entries {
                let label = egui::RichText::new(format!("{} {name}", icon::FILE))
                    .color(theme::TEXT_PRIMARY);
                builder.leaf(id.0, label);
            }
        });

    for action in actions {
        if let Action::SetSelected(selected) = action {
            for node in selected {
                if let Some((id, _)) = entries.iter().find(|(id, _)| id.0 == node) {
                    state.open_firmware(*id);
                }
            }
        }
    }
}

// ── Adapters ────────────────────────────────────────────────────────────

fn adapters_panel(ui: &mut egui::Ui, state: &mut AppState) {
    // Snapshot the unconfigured ports for the dropdown.
    let unconfigured: Vec<crate::port_registry::AvailablePort> = state
        .unconfigured_available_ports()
        .into_iter()
        .cloned()
        .collect();

    if let Some(idx) = state.new_port_selection {
        if idx >= unconfigured.len() {
            state.new_port_selection = None;
        }
    }

    taffy_tui(ui, egui::Id::new("adapters_panel_root"))
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
            // Heading and form.
            tui.style(Style {
                flex_shrink: 0.0,
                ..Default::default()
            })
            .ui(|ui| {
                ui.heading(format!("{} Serial Adapters", icon::PLUG));
                ui.separator();
                adapter_add_form(ui, state, &unconfigured);
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
            });

            // Port list fills remaining space.
            tui.style(Style {
                flex_grow: 1.0,
                min_size: Size { width: auto(), height: length(0.0) },
                ..Default::default()
            })
            .ui(|ui| {
                adapter_list(ui, state);
            });
        });
}

fn adapter_add_form(
    ui: &mut egui::Ui,
    state: &mut AppState,
    unconfigured: &[crate::port_registry::AvailablePort],
) {
    ui.horizontal(|ui| {
        ui.colored_label(theme::TEXT_SECONDARY, format!("{}", icon::PLUGS));
        ui.label("Port");

        let selected_text = match state.new_port_selection {
            Some(i) => unconfigured
                .get(i)
                .map(|p| p.label.clone())
                .unwrap_or_else(|| "—".to_string()),
            None => {
                if unconfigured.is_empty() {
                    "No USB ports available".to_string()
                } else {
                    "Select port…".to_string()
                }
            }
        };

        egui::ComboBox::from_id_salt("new_port_selection")
            .width(180.0)
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                for (i, port) in unconfigured.iter().enumerate() {
                    ui.selectable_value(
                        &mut state.new_port_selection,
                        Some(i),
                        port.label.clone(),
                    );
                }
            });

        if ui
            .small_button(icon::ARROWS_CLOCKWISE)
            .on_hover_text("Rescan USB serial ports")
            .clicked()
        {
            state.refresh_available_ports();
        }
    });
    ui.horizontal(|ui| {
        ui.colored_label(theme::TEXT_SECONDARY, format!("{}", icon::TAG));
        ui.label("Type");
        egui::ComboBox::from_id_salt("new_port_type")
            .width(80.0)
            .selected_text(match state.new_port_type {
                SerialAdapterType::Usart1 => "USART1",
                SerialAdapterType::Usart2 => "USART2",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut state.new_port_type,
                    SerialAdapterType::Usart1,
                    "USART1",
                );
                ui.selectable_value(
                    &mut state.new_port_type,
                    SerialAdapterType::Usart2,
                    "USART2",
                );
            });
    });

    let add_btn = egui::Button::new(format!("{} Add", icon::PLUS));
    if ui
        .add_enabled(state.new_port_selection.is_some(), add_btn)
        .clicked()
    {
        state.register_serial();
    }
}

/// Sidebar tree node IDs for the adapter list.
///
/// Bit 63 clear = port dir (USART2 only); the low bits hold the port index.
/// Bit 63 set = pane leaf; bits 0..2 encode the pane kind, bits 3..62 encode
/// the port index. USART1 ports are rendered as a leaf with id 0 (port dir).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct AdapterNodeId(u64);

impl AdapterNodeId {
    fn port(idx: usize) -> Self {
        AdapterNodeId(idx as u64)
    }
    fn pane(idx: usize, kind: u64) -> Self {
        AdapterNodeId((1 << 63) | ((idx as u64) << 3) | kind)
    }
    fn logs(idx: usize) -> Self {
        Self::pane(idx, 0)
    }
    fn control(idx: usize) -> Self {
        Self::pane(idx, 1)
    }
}

fn adapter_list(ui: &mut egui::Ui, state: &mut AppState) {
    if state.serial_ports.is_empty() {
        ui.colored_label(theme::TEXT_DIM, "No serial adapters configured");
        return;
    }

    let entries: Vec<(usize, String, SerialAdapterType, SerialPortStatus)> = state
        .serial_ports
        .iter()
        .enumerate()
        .map(|(i, cfg)| (i, cfg.port.clone(), cfg.adapter_type, cfg.status))
        .collect();

    let tree_id = ui.make_persistent_id("adapter_tree");
    let (_response, actions) = TreeView::new(tree_id)
        .allow_multi_selection(false)
        .fill_space_horizontal(true)
        .show(ui, |builder| {
            for (i, port, adapter_type, status) in &entries {
                let type_label = match adapter_type {
                    SerialAdapterType::Usart1 => "USART1",
                    SerialAdapterType::Usart2 => "USART2",
                };
                let (color, status_icon) = match status {
                    SerialPortStatus::Connecting => (theme::TEXT_DIM, icon::CIRCLE_NOTCH),
                    SerialPortStatus::PortOpen => (theme::WARN, icon::CIRCLE_HALF),
                    SerialPortStatus::DeviceDetected => (theme::INFO, icon::CHECK_CIRCLE),
                    SerialPortStatus::Error => (theme::ERROR, icon::WARNING_CIRCLE),
                };
                let label_text = format!("{status_icon} {port} [{type_label}]");
                let label = egui::RichText::new(label_text).color(color);

                match adapter_type {
                    SerialAdapterType::Usart1 => {
                        builder.leaf(AdapterNodeId::port(*i), label);
                    }
                    SerialAdapterType::Usart2 => {
                        builder.dir(AdapterNodeId::port(*i), label);
                        builder.leaf(
                            AdapterNodeId::logs(*i),
                            format!("{} Logs", icon::SCROLL),
                        );
                        builder.leaf(
                            AdapterNodeId::control(*i),
                            format!("{} Control", icon::TREE_STRUCTURE),
                        );
                        builder.close_dir();
                    }
                }
            }
        });

    for action in actions {
        if let Action::SetSelected(selected) = action {
            for node in selected {
                for (i, _, adapter_type, _) in &entries {
                    if node == AdapterNodeId::port(*i) {
                        match adapter_type {
                            SerialAdapterType::Usart1 => {
                                state.open_device_pane(Pane::SerialAdapter(*i));
                            }
                            SerialAdapterType::Usart2 => {
                                state.open_serial_port(*i);
                            }
                        }
                    } else if node == AdapterNodeId::logs(*i) {
                        state.open_device_pane(Pane::SerialAdapterLogs(*i));
                    } else if node == AdapterNodeId::control(*i) {
                        state.open_device_pane(Pane::SerialAdapterControl(*i));
                    }
                }
            }
        }
    }
}

// ── Devices ─────────────────────────────────────────────────────────────

/// Tree node IDs — device dirs and pane leaves packed into a u64.
/// Bit 63 clear = device dir, bit 63 set = pane leaf.
/// For panes, bits 0..2 encode the pane type, and bits 3..62 encode the device id.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct TreeNodeId(u64);

impl TreeNodeId {
    fn device(id: DeviceId) -> Self {
        TreeNodeId(id.0)
    }
    fn pane(id: DeviceId, kind: u64) -> Self {
        TreeNodeId((1 << 63) | (id.0 << 3) | kind)
    }
    fn logs(id: DeviceId) -> Self {
        Self::pane(id, 0)
    }
    fn protocol(id: DeviceId) -> Self {
        Self::pane(id, 1)
    }
    fn renode(id: DeviceId) -> Self {
        Self::pane(id, 2)
    }
}

fn devices_panel(ui: &mut egui::Ui, state: &mut AppState) {
    taffy_tui(ui, egui::Id::new("devices_panel_root"))
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
            tui.style(Style {
                flex_shrink: 0.0,
                ..Default::default()
            })
            .ui(|ui| {
                ui.heading(format!("{} Devices", icon::CPU));
                ui.separator();
            });

            tui.style(Style {
                flex_grow: 1.0,
                min_size: Size { width: auto(), height: length(0.0) },
                ..Default::default()
            })
            .ui(|ui| {
                devices_panel_body(ui, state);
            });
        });
}

fn devices_panel_body(ui: &mut egui::Ui, state: &mut AppState) {
    if state.devices.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            ui.colored_label(theme::TEXT_DIM, format!("{}", icon::BROADCAST));
            ui.colored_label(theme::TEXT_DIM, "No devices connected");
        });
        return;
    }

    let mut device_ids: Vec<DeviceId> = state.devices.keys().copied().collect();
    device_ids.sort_by_key(|id| {
        let dev = &state.devices[id];
        match dev.kind {
            DeviceKind::Persistent => 0,
            DeviceKind::Ephemeral => 1,
            DeviceKind::Emulator => 2,
        }
    });

    // Collect display info to avoid borrow conflicts.
    let entries: Vec<_> = device_ids
        .iter()
        .map(|id| {
            let dev = &state.devices[id];
            let kind_icon = match dev.kind {
                DeviceKind::Persistent => icon::HARD_DRIVE,
                DeviceKind::Ephemeral => icon::PLUG,
                DeviceKind::Emulator => icon::DESKTOP,
            };
            let phase_label = match dev.phase {
                DevicePhase::Stabilizing => Some(("STAB", theme::ERROR)),
                DevicePhase::Bootrom => Some(("ROM", theme::WARN)),
                DevicePhase::Bootloader => Some(("BL", theme::DEBUG)),
                DevicePhase::Kernel => Some(("OK", theme::INFO)),
                DevicePhase::Unknown => None,
            };
            (*id, dev.name.clone(), dev.is_connected(), dev.kind, kind_icon, phase_label)
        })
        .collect();

    let tree_id = ui.make_persistent_id("device_tree");
    let (_response, actions) = egui_ltreeview::TreeView::new(tree_id)
        .allow_multi_selection(false)
        .fill_space_horizontal(true)
        .show(ui, |builder| {
            for &(id, ref name, connected, kind, kind_icon, ref phase_label) in &entries {
                let color = if connected {
                    theme::INFO
                } else {
                    theme::TEXT_DIM
                };
                let label_text = if let Some((phase, _)) = phase_label {
                    format!("{kind_icon} {name} [{phase}]")
                } else {
                    format!("{kind_icon} {name}")
                };
                let label = egui::RichText::new(label_text).color(color);
                builder.dir(TreeNodeId::device(id), label);
                builder.leaf(TreeNodeId::logs(id), format!("{} Logs", icon::SCROLL));
                builder.leaf(TreeNodeId::protocol(id), format!("{} Protocol", icon::TREE_STRUCTURE));
                if kind == DeviceKind::Emulator {
                    builder.leaf(TreeNodeId::renode(id), format!("{} Renode", icon::DESKTOP));
                }
                builder.close_dir();
            }
        });

    for action in actions {
        if let egui_ltreeview::Action::SetSelected(selected) = action {
            for node in selected {
                // Device dir clicked → open all panes as tab group.
                if let Some(&(id, _, _, _, _, _)) = entries.iter().find(|(id, _, _, _, _, _)| {
                    TreeNodeId::device(*id) == node
                }) {
                    state.open_device(id);
                }
                // Leaf clicked → open individual pane.
                for &(id, _, _, _, _, _) in &entries {
                    if node == TreeNodeId::logs(id) {
                        state.open_device_pane(Pane::DeviceLogs(id));
                    } else if node == TreeNodeId::protocol(id) {
                        state.open_device_pane(Pane::DeviceProtocol(id));
                    } else if node == TreeNodeId::renode(id) {
                        state.open_device_pane(Pane::DeviceRenode(id));
                    }
                }
            }
        }
    }
}
