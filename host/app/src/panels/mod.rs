pub mod build_output;
pub mod device_renode;
pub mod firmware_status;
pub mod log_viewer;
pub mod placeholder;

pub mod serial_adapter;

use crate::state::{AppState, BuildId, DeviceId, FirmwareId, SerialPortIndex};
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
    /// Firmware status / details view.
    FirmwareStatus(FirmwareId),
    /// Build output / progress view.
    Build(BuildId),
    /// Serial adapter management view.
    SerialAdapter(SerialPortIndex),
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
            Pane::FirmwareStatus(id) => {
                let name = state
                    .firmware
                    .get(id)
                    .map(|fw| fw.display_name())
                    .unwrap_or_else(|| "Firmware".into());
                format!("{} {name}", icon::PACKAGE)
            }
            Pane::Build(id) => state
                .builds
                .get(id)
                .map(|b| format!("{} Build: {}", icon::HAMMER, b.config.config))
                .unwrap_or_else(|| format!("{} Build", icon::HAMMER)),
            Pane::SerialAdapter(idx) => state
                .serial_ports
                .get(*idx)
                .map(|cfg| format!("{} {}", icon::PLUG, cfg.port))
                .unwrap_or_else(|| format!("{} Adapter", icon::PLUG)),
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
            Pane::FirmwareStatus(id) => {
                let fw_id = *id;
                if let Some(fw) = self.state.firmware.get(id) {
                    match firmware_status::show(ui, fw) {
                        firmware_status::FirmwareAction::RunEmulator => {
                            self.state.run_emulator(fw_id);
                        }
                        firmware_status::FirmwareAction::Flash => {
                            self.state.flash_modal = Some(crate::state::FlashModalState::Picker {
                                firmware_id: fw_id,
                                selected_device: None,
                            });
                        }
                        firmware_status::FirmwareAction::None => {}
                    }
                } else {
                    ui.label("Firmware not found");
                }
            }
            Pane::Build(id) => {
                if let Some(build) = self.state.builds.get(id) {
                    build_output::show(ui, build);
                } else {
                    ui.label("Build not found");
                }
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
