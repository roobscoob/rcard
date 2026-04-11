use crate::bridge::FlashPhase;
use crate::panels::{self, PaneBehavior};
use crate::sidebar;
use crate::state::*;
use crate::theme;
use egui_phosphor::regular as icon;

/// Get the cursor position in screen coordinates during an OS file drag.
/// On Windows, `CursorMoved` events stop during OLE drag-drop, so we
/// query the OS directly.
fn get_cursor_pos_in_window(ctx: &egui::Context) -> Option<egui::Pos2> {
    #[cfg(target_os = "windows")]
    {
        use std::mem::MaybeUninit;
        #[repr(C)]
        struct POINT {
            x: i32,
            y: i32,
        }
        unsafe extern "system" {
            fn GetCursorPos(point: *mut POINT) -> i32;
        }

        let mut point = MaybeUninit::<POINT>::uninit();
        let ok = unsafe { GetCursorPos(point.as_mut_ptr()) };
        if ok == 0 {
            return None;
        }
        let point = unsafe { point.assume_init() };

        // Convert from screen coords to egui points.
        // We need the window position and scale factor.
        let viewport = ctx.input(|i| i.viewport().clone());
        let window_pos = viewport.inner_rect.map(|r| r.min)?;
        let ppp = ctx.pixels_per_point();

        Some(egui::pos2(
            (point.x as f32 - window_pos.x) / ppp,
            (point.y as f32 - window_pos.y) / ppp,
        ))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = ctx;
        None
    }
}

pub struct RcardApp {
    pub state: AppState,
    runtime: Option<tokio::runtime::Runtime>,
}

impl RcardApp {
    pub fn new(
        cmd_tx: tokio::sync::mpsc::UnboundedSender<crate::bridge::Command>,
        event_rx: crossbeam_channel::Receiver<crate::bridge::Event>,
        _ctx: egui::Context,
        runtime: tokio::runtime::Runtime,
    ) -> Self {
        let mut state = AppState::new(cmd_tx, event_rx);
        state.scan_firmware_db();

        RcardApp {
            state,
            runtime: Some(runtime),
        }
    }
}

impl Drop for RcardApp {
    fn drop(&mut self) {
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_background();
        }
    }
}

impl eframe::App for RcardApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // During OS file drag on Windows, CursorMoved events stop.
        // Query GetCursorPos and inject pointer events so egui_tiles
        // can compute drop zones and detect the drop.
        let is_hovering = !raw_input.hovered_files.is_empty();
        let is_dropping = !raw_input.dropped_files.is_empty();

        // Inject pointer position during OLE drag. file_drag is set in
        // update() which runs after this hook, so on the first hover frame
        // it's None. Use hovered_files as the trigger instead — if .tfw
        // files are hovering, we'll need pointer events.
        let has_tfw = raw_input.hovered_files.iter().any(|f| {
            f.path
                .as_ref()
                .is_some_and(|p| p.extension().is_some_and(|e| e == "tfw"))
        });
        if has_tfw || ((is_hovering || is_dropping) && self.state.file_drag.is_some()) {
            if let Some(pos) = get_cursor_pos_in_window(ctx) {
                raw_input.events.push(egui::Event::PointerMoved(pos));

                if is_dropping {
                    raw_input.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
            }
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.state.drain_events();

        // ── File drop handling ──────────────────────────────────────
        // On hover: load the .tfw, insert a real pane, set it as dragged
        // so egui_tiles shows the drop preview. On drop: pane stays.
        // On cancel: remove pane + firmware.
        let hovered_files = ctx.input(|i| i.raw.hovered_files.clone());
        let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
        let is_hovering = !hovered_files.is_empty();
        let is_dropping = !dropped_files.is_empty();

        if is_hovering && self.state.file_drag.is_none() {
            let tfw_path = hovered_files.iter().find_map(|f| {
                f.path
                    .as_ref()
                    .filter(|p| p.extension().is_some_and(|ext| ext == "tfw"))
                    .cloned()
            });

            if let Some(path) = tfw_path {
                eprintln!("[drag] hover started: {}", path.display());
                match self.state.load_firmware(path) {
                    Ok(fw_id) => {
                        let pane = panels::Pane::FirmwareStatus(fw_id);
                        let tile_id = self.state.tree.tiles.insert_pane(pane);

                        if self.state.tree.root().is_none() {
                            eprintln!("[drag] no root, creating tab");
                            let tab = self.state.tree.tiles.insert_tab_tile(vec![tile_id]);
                            self.state.tree.root = Some(tab);
                        } else {
                            eprintln!("[drag] has root, attaching invisible");
                            self.state.tree.set_visible(tile_id, false);
                            if let Some(root) = self.state.tree.root() {
                                if let Some(egui_tiles::Tile::Container(c)) =
                                    self.state.tree.tiles.get_mut(root)
                                {
                                    c.add_child(tile_id);
                                }
                            }
                        }

                        let tree_egui_id = egui::Id::new("main_tree");
                        let drag_id = tile_id.egui_id(tree_egui_id);
                        eprintln!("[drag] set_dragged_id: tile={tile_id:?} egui_id={drag_id:?}");
                        ctx.set_dragged_id(drag_id);

                        self.state.file_drag = Some(FileDragState {
                            firmware_id: fw_id,
                            tile_id,
                            dropped: false,
                        });
                    }
                    Err(e) => eprintln!("[drag] load failed: {e}"),
                }
            } else {
                eprintln!("[drag] hover but no .tfw found");
            }
        } else if is_dropping {
            eprintln!("[drag] drop, file_drag={}", self.state.file_drag.is_some());
            if let Some(drag) = &mut self.state.file_drag {
                drag.dropped = true;
            }
            ctx.request_repaint();
        } else if !is_hovering && !is_dropping {
            if let Some(drag) = self.state.file_drag.take() {
                eprintln!("[drag] cleanup, dropped={}", drag.dropped);
                ctx.stop_dragging();
                if drag.dropped {
                    self.state.tree.set_visible(drag.tile_id, true);
                } else {
                    self.state.tree.remove_recursively(drag.tile_id);
                    self.state.firmware.remove(&drag.firmware_id);
                }
            }
        }

        // ── Activity bar (narrow icon strip, far left) ──────────────
        if self.state.file_drag.is_some() {
            let dragged = ctx.dragged_id();
            let ptr = ctx.input(|i| i.pointer.hover_pos());
            eprintln!(
                "[drag] frame: dragged_id={dragged:?} pointer={ptr:?} hovering={is_hovering} dropping={is_dropping}"
            );
        }

        // ── Activity bar (narrow icon strip, far left) ──────────────
        egui::SidePanel::left("activity_bar")
            .exact_width(44.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(egui::Color32::from_rgb(0x16, 0x18, 0x24)))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(10.0);
                    activity_button(
                        ui,
                        icon::PACKAGE,
                        "Firmware",
                        &mut self.state.sidebar_section,
                        SidebarSection::Firmware,
                    );
                    activity_button(
                        ui,
                        icon::PLUG,
                        "Adapters",
                        &mut self.state.sidebar_section,
                        SidebarSection::Adapters,
                    );
                    activity_button(
                        ui,
                        icon::CPU,
                        "Devices",
                        &mut self.state.sidebar_section,
                        SidebarSection::Devices,
                    );
                });
            });

        // ── Sidebar panel (changes with activity bar selection) ─────
        egui::SidePanel::left("sidebar")
            .default_width(220.0)
            .resizable(true)
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(0x1E, 0x20, 0x30))
                    .inner_margin(egui::Margin::same(8)),
            )
            .show(ctx, |ui| {
                sidebar::show(ui, &mut self.state);
            });

        // ── Main area (tiled panels via egui_tiles) ─────────────────
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(0x23, 0x25, 0x36))
                    .inner_margin(egui::Margin::same(0)),
            )
            .show(ctx, |ui| {
                if self.state.tree.root().is_none() {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(ui.available_height() / 2.0 - 40.0);
                            ui.label(
                                egui::RichText::new(icon::MONITOR)
                                    .size(48.0)
                                    .color(theme::TEXT_DIM),
                            );
                            ui.add_space(8.0);
                            ui.colored_label(
                                theme::TEXT_DIM,
                                "Select a device or drop a .tfw file to get started",
                            );
                        });
                    });
                    return;
                }

                let mut behavior = PaneBehavior {
                    state: &mut self.state,
                };
                let mut tree =
                    std::mem::replace(&mut behavior.state.tree, egui_tiles::Tree::empty("temp"));
                tree.ui(&mut behavior, ui);
                behavior.state.tree = tree;
            });

        // ── Flash modal ────────────────────────────────────────────────
        if self.state.flash_modal.is_some() {
            let mut close = false;

            // Dim the background behind the modal.
            let screen_rect = ctx.screen_rect();
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("flash_modal_backdrop"),
            ))
            .rect_filled(screen_rect, 0.0, egui::Color32::from_black_alpha(120));

            let is_flashing = matches!(
                self.state.flash_modal,
                Some(FlashModalState::Flashing { .. })
            );
            egui::Window::new(format!("{} Flash Firmware", icon::LIGHTNING))
                .collapsible(false)
                .resizable(is_flashing)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .min_width(if is_flashing { 500.0 } else { 340.0 })
                .min_height(if is_flashing { 100.0 } else { 0.0 })
                .show(ctx, |ui| {
                    match self.state.flash_modal.as_ref().unwrap() {
                        FlashModalState::Picker {
                            firmware_id,
                            selected_device,
                        } => {
                            let firmware_id = *firmware_id;
                            let selected_device = *selected_device;
                            let fw_name = self
                                .state
                                .firmware
                                .get(&firmware_id)
                                .map(|fw| fw.display_name())
                                .unwrap_or_else(|| "?".into());
                            let device_entries: Vec<(DeviceId, String)> = self
                                .state
                                .devices
                                .iter()
                                .map(|(id, dev)| (*id, dev.name.clone()))
                                .collect();
                            let method = selected_device
                                .and_then(|id| self.state.flash_method_for_device(id));
                            let tfw_path = self
                                .state
                                .firmware
                                .get(&firmware_id)
                                .map(|fw| fw.path.clone());

                            ui.horizontal(|ui| {
                                ui.colored_label(
                                    theme::TEXT_SECONDARY,
                                    format!("{} Firmware:", icon::PACKAGE),
                                );
                                ui.strong(&fw_name);
                            });
                            ui.add_space(8.0);

                            ui.colored_label(
                                theme::TEXT_SECONDARY,
                                format!("{} Target device:", icon::CPU),
                            );

                            if device_entries.is_empty() {
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.colored_label(theme::TEXT_DIM, "No devices connected");
                                });
                            } else {
                                let flash = self.state.flash_modal.as_mut().unwrap();
                                if let FlashModalState::Picker {
                                    selected_device: sel,
                                    ..
                                } = flash
                                {
                                    for (id, name) in &device_entries {
                                        let selected = *sel == Some(*id);
                                        if ui.selectable_label(selected, name).clicked() {
                                            *sel = Some(*id);
                                        }
                                    }
                                }
                            }

                            ui.add_space(8.0);

                            if let Some(device_id) = selected_device {
                                match method {
                                    Some(FlashMethod::Usb) => {
                                        ui.horizontal(|ui| {
                                            ui.colored_label(theme::INFO, icon::USB);
                                            ui.colored_label(
                                                theme::INFO,
                                                "USB (via existing firmware)",
                                            );
                                        });
                                    }
                                    Some(FlashMethod::SifliDebug) => {
                                        ui.horizontal(|ui| {
                                            ui.colored_label(theme::WARN, icon::PLUG);
                                            ui.colored_label(theme::WARN, "SifliDebug (USART1)");
                                        });
                                    }
                                    None => {
                                        ui.horizontal(|ui| {
                                            ui.colored_label(theme::ERROR, icon::WARNING);
                                            ui.colored_label(
                                                theme::ERROR,
                                                "No USB or USART1 adapter",
                                            );
                                        });
                                    }
                                }

                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    let flash_btn = egui::Button::new(egui::RichText::new(
                                        format!("{} Flash", icon::LIGHTNING),
                                    ));
                                    if ui.add_enabled(method.is_some(), flash_btn).clicked() {
                                        if let Some(tfw_path) = tfw_path {
                                            match method.unwrap() {
                                                FlashMethod::SifliDebug => {
                                                    let _ = self.state.cmd_tx.send(
                                                        crate::bridge::Command::FlashViaSifliDebug {
                                                            device_id,
                                                            firmware_id,
                                                            tfw_path,
                                                        },
                                                    );
                                                }
                                                FlashMethod::Usb => {
                                                    // TODO: USB flash path
                                                }
                                            }
                                        }
                                        // Don't close — modal transitions to Flashing
                                        // when FlashProgress(WaitingForReset) arrives.
                                    }
                                    if ui.button("Cancel").clicked() {
                                        close = true;
                                    }
                                });
                            } else {
                                ui.add_space(8.0);
                                if ui.button("Cancel").clicked() {
                                    close = true;
                                }
                            }
                        }

                        FlashModalState::Flashing {
                            firmware_id,
                            device_id,
                            phase,
                        } => {
                            let fw_name = self
                                .state
                                .firmware
                                .get(firmware_id)
                                .map(|fw| fw.display_name())
                                .unwrap_or_else(|| "?".into());
                            let dev_name = self
                                .state
                                .devices
                                .get(device_id)
                                .map(|d| d.name.clone())
                                .unwrap_or_else(|| "?".into());
                            let device_id = *device_id;

                            ui.horizontal(|ui| {
                                ui.colored_label(
                                    theme::TEXT_SECONDARY,
                                    format!("{} Firmware:", icon::PACKAGE),
                                );
                                ui.strong(&fw_name);
                            });
                            ui.horizontal(|ui| {
                                ui.colored_label(
                                    theme::TEXT_SECONDARY,
                                    format!("{} Device:", icon::CPU),
                                );
                                ui.strong(&dev_name);
                            });
                            ui.add_space(12.0);

                            flash_modal_steps(ui, phase);

                            if matches!(phase, FlashPhase::Booting | FlashPhase::Done) {
                                ui.add_space(8.0);
                                ui.separator();
                                if let Some(dev) = self.state.devices.get(&device_id) {
                                    panels::log_viewer::show(ui, dev, &self.state);
                                }
                            }
                        }
                    }
                });

            // Check for close signal from the flash steps close button.
            let steps_close = ctx.memory(|mem| {
                mem.data
                    .get_temp::<bool>(egui::Id::new("flash_modal_close"))
                    .unwrap_or(false)
            });
            if steps_close {
                ctx.memory_mut(|mem| mem.data.remove::<bool>(egui::Id::new("flash_modal_close")));
                close = true;
            }

            if close {
                self.state.flash_modal = None;
            }
        }
    }
}

/// A single activity bar icon button.
fn activity_button(
    ui: &mut egui::Ui,
    icon: &str,
    tooltip: &str,
    current: &mut SidebarSection,
    target: SidebarSection,
) {
    let is_active = *current == target;
    let color = if is_active {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_DIM
    };

    let button = egui::Button::new(egui::RichText::new(icon).size(22.0).color(color))
        .fill(egui::Color32::TRANSPARENT)
        .frame(false);

    let response = ui.add(button).on_hover_text(tooltip);

    if response.clicked() {
        *current = target;
    }

    if is_active {
        let rect = response.rect;
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left() - 6.0, rect.top() + 2.0),
                egui::vec2(2.5, rect.height() - 4.0),
            ),
            1.0,
            theme::ACCENT,
        );
    }
}

/// Render the flash progress steps inside the modal.
fn flash_modal_steps(ui: &mut egui::Ui, phase: &FlashPhase) {
    // Step index for the current phase.
    let step = match phase {
        FlashPhase::Resetting | FlashPhase::WaitingForReset => 0,
        FlashPhase::Writing { .. } => 1,
        FlashPhase::Booting => 2,
        FlashPhase::Done | FlashPhase::Failed(_) => 3,
    };

    let step0_label = match phase {
        FlashPhase::WaitingForReset => "Waiting for device reset...",
        _ => "Resetting device...",
    };

    let labels = [step0_label, "Writing stub to RAM", "Booting stub..."];

    for (i, label) in labels.iter().enumerate() {
        if i < step {
            ui.horizontal(|ui| {
                ui.colored_label(theme::INFO, icon::CHECK_CIRCLE);
                ui.colored_label(theme::INFO, *label);
            });
        } else if i == step {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.0).color(theme::ACCENT));
                ui.colored_label(theme::ACCENT, *label);
            });
        } else {
            ui.horizontal(|ui| {
                ui.colored_label(theme::TEXT_DIM, icon::CIRCLE);
                ui.colored_label(theme::TEXT_DIM, *label);
            });
        }

        // Helper text asking for a manual reset.
        if i == 0 && i == step && matches!(phase, FlashPhase::WaitingForReset) {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.colored_label(
                    theme::TEXT_DIM,
                    "Couldn't auto-reset — please reset your device.",
                );
            });
            ui.add_space(2.0);
        }

        // Progress bar for the writing step.
        if i == 1 {
            if let FlashPhase::Writing {
                bytes_written,
                bytes_total,
            } = phase
            {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    let fraction = if *bytes_total > 0 {
                        *bytes_written as f32 / *bytes_total as f32
                    } else {
                        0.0
                    };
                    let bar = egui::ProgressBar::new(fraction).text(format!(
                        "{:.1} / {:.1} KB",
                        *bytes_written as f32 / 1024.0,
                        *bytes_total as f32 / 1024.0,
                    ));
                    ui.add(bar);
                });
                ui.add_space(2.0);
            }
        }
    }

    // Done / Failed state.
    match phase {
        FlashPhase::Done => {
            ui.add_space(4.0);
            ui.colored_label(theme::INFO, "Complete!");
        }
        FlashPhase::Failed(err) => {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.colored_label(theme::ERROR, icon::X_CIRCLE);
                ui.colored_label(theme::ERROR, "Failed");
            });
            ui.add_space(4.0);
            ui.colored_label(theme::ERROR, err);
        }
        _ => {}
    }

    // Close button on terminal states.
    if matches!(phase, FlashPhase::Done | FlashPhase::Failed(_)) {
        ui.add_space(8.0);
        // Can't close here directly (no &mut to flash_modal). We'll use
        // a memory flag that the caller checks.
        // Actually — we return and the caller handles close via the
        // window's close button. But let's add an explicit button.
        // We need to signal close to the caller. Use ui.memory.
        if ui.button("Close").clicked() {
            ui.memory_mut(|mem| {
                mem.data
                    .insert_temp(egui::Id::new("flash_modal_close"), true)
            });
        }
    }
}
