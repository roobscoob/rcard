pub mod build_output;
pub mod device_renode;
pub mod log_viewer;
pub mod placeholder;
pub mod raw_logs;
pub mod raw_terminal;
pub mod serial_control;

pub mod serial_adapter;

use crate::state::{AppState, BuildId, DeviceId, SerialPortIndex};
use egui_phosphor::regular as icon;

/// A pane in the tiled main area.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pane {
    /// Device log viewer.
    DeviceLogs(DeviceId),
    /// Device protocol inspector.
    DeviceProtocol(DeviceId),
    /// Renode emulator management view (emulator devices only).
    DeviceRenode(DeviceId),
    /// Unified build / firmware panel. The `BuildId` addresses a
    /// `BuildHandle` in `AppState::builds`, which can be either a
    /// live build or a snapshot synthesised from a loaded `.tfw`
    /// archive. The panel never branches on which — it renders
    /// whatever's in the handle.
    Firmware(BuildId),
    /// USART1 serial adapter management view (raw terminal).
    SerialAdapter(SerialPortIndex),
    /// USART2 Logs sub-pane — decoded structured log entries.
    SerialAdapterLogs(SerialPortIndex),
    /// USART2 Control sub-pane — decoded IPC replies / tunnel errors.
    SerialAdapterControl(SerialPortIndex),
}

impl Pane {
    pub fn title(&self, state: &AppState) -> String {
        match self {
            Pane::DeviceLogs(id) => {
                let name = state
                    .devices
                    .get(id)
                    .map(|d| d.name.as_str())
                    .unwrap_or("?");
                format!("{} {name} — Logs", icon::SCROLL)
            }
            Pane::DeviceProtocol(id) => {
                let name = state
                    .devices
                    .get(id)
                    .map(|d| d.name.as_str())
                    .unwrap_or("?");
                format!("{} {name} — Protocol", icon::TREE_STRUCTURE)
            }
            Pane::DeviceRenode(id) => {
                let name = state
                    .devices
                    .get(id)
                    .map(|d| d.name.as_str())
                    .unwrap_or("?");
                format!("{} {name} — Renode", icon::DESKTOP)
            }
            Pane::Firmware(id) => {
                if let Some(b) = state.builds.get(id) {
                    let glyph = match b.status {
                        crate::state::BuildStatus::Running => icon::HAMMER,
                        crate::state::BuildStatus::Succeeded { .. } => icon::PACKAGE,
                        crate::state::BuildStatus::Failed { .. } => icon::X_CIRCLE,
                    };
                    format!("{} {}", glyph, b.config.config)
                } else {
                    format!("{} Firmware", icon::PACKAGE)
                }
            }
            Pane::SerialAdapter(idx) => state
                .serial_ports
                .get(*idx)
                .map(|cfg| format!("{} {}", icon::PLUG, cfg.port))
                .unwrap_or_else(|| format!("{} Adapter", icon::PLUG)),
            Pane::SerialAdapterLogs(idx) => state
                .serial_ports
                .get(*idx)
                .map(|cfg| format!("{} {} — Logs", icon::PLUG, cfg.port))
                .unwrap_or_else(|| format!("{} Logs", icon::PLUG)),
            Pane::SerialAdapterControl(idx) => state
                .serial_ports
                .get(*idx)
                .map(|cfg| format!("{} {} — Control", icon::PLUG, cfg.port))
                .unwrap_or_else(|| format!("{} Control", icon::PLUG)),
        }
    }
}

/// Behavior implementation for egui_tiles.
pub struct PaneBehavior<'a> {
    pub state: &'a mut AppState,
}

impl<'a> egui_tiles::Behavior<Pane> for PaneBehavior<'a> {
    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        _tile_id: egui_tiles::TileId,
        pane: &mut Pane,
    ) -> egui_tiles::UiResponse {
        match pane {
            Pane::DeviceLogs(id) => {
                if let Some(dev) = self.state.devices.get(id) {
                    log_viewer::show(ui, dev, self.state);
                } else {
                    ui.label("Device not found");
                }
            }
            Pane::DeviceProtocol(_id) => {
                placeholder::show(ui, "Protocol Inspector");
            }
            Pane::DeviceRenode(id) => {
                let id = *id;
                if let Some(dev) = self.state.devices.get(&id) {
                    match device_renode::show(ui, dev) {
                        device_renode::RenodeAction::Kill => {
                            let _ = self.state.cmd_tx.send(
                                crate::bridge::Command::RemoveDevice(id),
                            );
                        }
                        device_renode::RenodeAction::None => {}
                    }
                } else {
                    ui.label("Device not found");
                }
            }
            Pane::Firmware(build_id) => {
                // Single dispatch point. Whether this `BuildHandle` was
                // produced by a live build or synthesised from an
                // archive is irrelevant to the renderer — it just
                // reads `BuildHandle` fields.
                let action = if let Some(build) = self.state.builds.get(build_id) {
                    build_output::show(ui, build)
                } else {
                    ui.label("Firmware not found");
                    build_output::PanelAction::None
                };
                handle_panel_action(self.state, action);
            }
            Pane::SerialAdapter(idx) => {
                let idx = *idx;
                if let Some(cfg) = self.state.serial_ports.get(idx) {
                    match serial_adapter::show(ui, cfg) {
                        serial_adapter::SerialAdapterAction::Detach => {
                            self.state.unregister_serial(idx);
                        }
                        serial_adapter::SerialAdapterAction::None => {}
                    }
                } else {
                    ui.label("Adapter not found");
                }
            }
            Pane::SerialAdapterLogs(idx) => {
                let idx = *idx;
                if let Some(cfg) = self.state.serial_ports.get(idx) {
                    match serial_adapter::show_logs(ui, cfg) {
                        serial_adapter::SerialAdapterAction::Detach => {
                            self.state.unregister_serial(idx);
                        }
                        serial_adapter::SerialAdapterAction::None => {}
                    }
                } else {
                    ui.label("Adapter not found");
                }
            }
            Pane::SerialAdapterControl(idx) => {
                let idx = *idx;
                if let Some(cfg) = self.state.serial_ports.get(idx) {
                    match serial_adapter::show_control(ui, cfg) {
                        serial_adapter::SerialAdapterAction::Detach => {
                            self.state.unregister_serial(idx);
                        }
                        serial_adapter::SerialAdapterAction::None => {}
                    }
                } else {
                    ui.label("Adapter not found");
                }
            }
        }
        egui_tiles::UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        pane.title(self.state).into()
    }

    fn is_tab_closable(
        &self,
        _tiles: &egui_tiles::Tiles<Pane>,
        _tile_id: egui_tiles::TileId,
    ) -> bool {
        true
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        egui_tiles::SimplificationOptions {
            all_panes_must_have_tabs: true,
            ..Default::default()
        }
    }
}

/// Dispatch a `PanelAction` returned by the unified Build/Firmware panel.
fn handle_panel_action(state: &mut AppState, action: build_output::PanelAction) {
    match action {
        build_output::PanelAction::None => {}
        build_output::PanelAction::Flash(fw_id) => {
            state.flash_modal = Some(crate::state::FlashModalState::Picker {
                firmware_id: fw_id,
                selected_device: None,
            });
        }
        build_output::PanelAction::RunEmulator(fw_id) => {
            state.run_emulator(fw_id);
        }
        build_output::PanelAction::DeleteBuild(build_id) => {
            state.remove_build(build_id);
        }
    }
}
