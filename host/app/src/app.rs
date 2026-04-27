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
        let mut state = AppState::new(cmd_tx, event_rx, runtime.handle().clone());
        state.register_builtin_stub();
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
                        // Resolve to (or synthesize) a BuildHandle so
                        // the pane can render through the unified
                        // BuildId path.
                        let Some(build_id) = self.state.build_for_firmware(fw_id) else {
                            return;
                        };
                        let pane = panels::Pane::Firmware(build_id);
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
            // Cap the window so the log viewer can't keep pushing it taller
            // each frame. Within this cap the user can still resize freely
            // and the log viewer will fill the extra space.
            let max_h = (ctx.screen_rect().height() * 0.8).max(300.0);
            let window_id = if is_flashing {
                egui::Id::new("flash_modal_flashing_window")
            } else {
                egui::Id::new("flash_modal_picker_window")
            };
            egui::Window::new(format!("{} Flash Firmware", icon::LIGHTNING))
                .id(window_id)
                .collapsible(false)
                .resizable(is_flashing)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .min_width(if is_flashing { 500.0 } else { 340.0 })
                .min_height(if is_flashing { 100.0 } else { 0.0 })
                .max_height(max_h)
                .default_height(if is_flashing { 480.0 } else { 0.0 })
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
                                .and_then(|fw| fw.path.clone());

                            use egui_taffy::taffy::prelude::*;
                            use egui_taffy::{tui as taffy_tui, TuiBuilderLogic};

                            taffy_tui(ui, egui::Id::new("flash_modal_picker"))
                                .reserve_available_width()
                                .style(Style {
                                    display: Display::Flex,
                                    flex_direction: FlexDirection::Column,
                                    align_items: Some(AlignItems::Stretch),
                                    size: Size { width: percent(1.0), height: auto() },
                                    gap: length(8.0),
                                    ..Default::default()
                                })
                                .show(|tui| {
                                    tui.ui(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.colored_label(
                                                theme::TEXT_SECONDARY,
                                                format!("{} Firmware:", icon::PACKAGE),
                                            );
                                            ui.strong(&fw_name);
                                        });
                                    });

                                    tui.ui(|ui| {
                                        ui.colored_label(
                                            theme::TEXT_SECONDARY,
                                            format!("{} Target device:", icon::CPU),
                                        );
                                    });

                                    if device_entries.is_empty() {
                                        tui.ui(|ui| {
                                            ui.horizontal(|ui| {
                                                ui.add_space(16.0);
                                                ui.colored_label(
                                                    theme::TEXT_DIM,
                                                    "No devices connected",
                                                );
                                            });
                                        });
                                    } else {
                                        tui.ui(|ui| {
                                            let flash = self.state.flash_modal.as_mut().unwrap();
                                            if let FlashModalState::Picker {
                                                selected_device: sel,
                                                ..
                                            } = flash
                                            {
                                                for (id, name) in &device_entries {
                                                    let selected = *sel == Some(*id);
                                                    if ui
                                                        .selectable_label(selected, name)
                                                        .clicked()
                                                    {
                                                        *sel = Some(*id);
                                                    }
                                                }
                                            }
                                        });
                                    }

                                    if let Some(_) = selected_device {
                                        tui.ui(|ui| match method {
                                            Some(FlashMethod::Ipc { transport }) => {
                                                ui.horizontal(|ui| {
                                                    ui.colored_label(theme::INFO, icon::USB);
                                                    ui.colored_label(
                                                        theme::INFO,
                                                        format!(
                                                            "{} (via existing firmware IPC)",
                                                            transport.to_uppercase(),
                                                        ),
                                                    );
                                                });
                                            }
                                            Some(FlashMethod::SifliDebug) => {
                                                ui.horizontal(|ui| {
                                                    ui.colored_label(theme::WARN, icon::PLUG);
                                                    ui.colored_label(
                                                        theme::WARN,
                                                        "SifliDebug (USART1)",
                                                    );
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
                                        });
                                    }

                                    // Footer row: Flash + Cancel, right-aligned.
                                    tui.style(Style {
                                        display: Display::Flex,
                                        flex_direction: FlexDirection::Row,
                                        align_items: Some(AlignItems::Center),
                                        gap: length(8.0),
                                        ..Default::default()
                                    })
                                    .add(|tui| {
                                        tui.style(Style {
                                            flex_grow: 1.0,
                                            ..Default::default()
                                        })
                                        .add_empty();

                                        if let Some(device_id) = selected_device {
                                            // Temporary: manual MoshiMoshi probe trigger.
                                            // Bypasses the auto-fire-on-connect behavior
                                            // so we can re-fire without restarting the app.
                                            tui.ui(|ui| {
                                                let moshi_btn = egui::Button::new("MoshiMoshi");
                                                if ui
                                                    .add_enabled(
                                                        matches!(method, Some(FlashMethod::Ipc { .. })),
                                                        moshi_btn,
                                                    )
                                                    .on_hover_text(
                                                        "Fire a MoshiMoshi probe on USART2 \
                                                         (manual re-trigger for the USART1 hello)."
                                                    )
                                                    .clicked()
                                                {
                                                    self.state.send_moshi_moshi(device_id);
                                                }
                                            });

                                            tui.ui(|ui| {
                                                let flash_btn = egui::Button::new(
                                                    egui::RichText::new(format!(
                                                        "{} Flash",
                                                        icon::LIGHTNING
                                                    )),
                                                );
                                                if ui
                                                    .add_enabled(method.is_some(), flash_btn)
                                                    .clicked()
                                                {
                                                    if let Some(tfw_path) = tfw_path.clone() {
                                                        if let Some(FlashMethod::SifliDebug) =
                                                            method
                                                        {
                                                            let _ = self.state.cmd_tx.send(
                                                                crate::bridge::Command::FlashViaSifliDebug {
                                                                    device_id,
                                                                    firmware_id,
                                                                    tfw_path,
                                                                },
                                                            );
                                                        }
                                                    }
                                                }
                                            });
                                        }

                                        tui.ui(|ui| {
                                            if ui.button("Cancel").clicked() {
                                                close = true;
                                            }
                                        });
                                    });
                                });
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
                            let phase_terminal = matches!(
                                phase,
                                FlashPhase::BootingStub
                                    | FlashPhase::StubBooted
                                    | FlashPhase::Erasing
                                    | FlashPhase::Programming { .. }
                                    | FlashPhase::Verifying { .. }
                                    | FlashPhase::Done
                                    | FlashPhase::Failed { .. }
                            );
                            let show_logs = matches!(
                                phase,
                                FlashPhase::BootingStub
                                    | FlashPhase::StubBooted
                                    | FlashPhase::Erasing
                                    | FlashPhase::Programming { .. }
                                    | FlashPhase::Verifying { .. }
                                    | FlashPhase::Done
                                    | FlashPhase::Failed { .. }
                            );

                            use egui_taffy::taffy::prelude::*;
                            use egui_taffy::{tui as taffy_tui, TuiBuilderLogic};

                            taffy_tui(ui, egui::Id::new("flash_modal_flashing"))
                                .style(Style {
                                    display: Display::Flex,
                                    flex_direction: FlexDirection::Column,
                                    align_items: Some(AlignItems::Stretch),
                                    gap: length(6.0),
                                    ..Default::default()
                                })
                                .show(|tui| {
                                    // Header block: fw/device names + steps.
                                    tui.style(Style {
                                        flex_shrink: 0.0,
                                        ..Default::default()
                                    })
                                    .ui(|ui| {
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
                                        ui.add_space(6.0);
                                        flash_modal_steps(ui, phase);
                                    });

                                    // Body: log viewer — starts at 300px, grows into
                                    // any extra space the user gives by resizing.
                                    if show_logs {
                                        tui.separator();
                                        tui.style(Style {
                                            flex_grow: 1.0,
                                            min_size: Size {
                                                width: auto(),
                                                height: length(300.0),
                                            },
                                            ..Default::default()
                                        })
                                        .ui(|ui| {
                                            if let Some(dev) =
                                                self.state.devices.get(&device_id)
                                            {
                                                panels::log_viewer::show(ui, dev, &self.state);
                                            }
                                        });
                                    }

                                    // Footer: cancel / close button, right-aligned.
                                    tui.separator();
                                    tui.style(Style {
                                        display: Display::Flex,
                                        flex_direction: FlexDirection::Row,
                                        align_items: Some(AlignItems::Center),
                                        flex_shrink: 0.0,
                                        padding: length(4.0),
                                        ..Default::default()
                                    })
                                    .add(|tui| {
                                        tui.style(Style {
                                            flex_grow: 1.0,
                                            ..Default::default()
                                        })
                                        .add_empty();

                                        // Temporary: MoshiMoshi re-trigger button.
                                        // Most useful while the flash is mid-flight
                                        // (waiting for USB reattach after stub boot) —
                                        // lets us probe without cancelling the flow.
                                        tui.ui(|ui| {
                                            if ui
                                                .button("MoshiMoshi")
                                                .on_hover_text(
                                                    "Fire a MoshiMoshi probe on USART2 \
                                                     (manual re-trigger for the USART1 hello)."
                                                )
                                                .clicked()
                                            {
                                                self.state.send_moshi_moshi(device_id);
                                            }
                                        });

                                        let label =
                                            if phase_terminal { "Close" } else { "Cancel" };
                                        if tui.ui_add(egui::Button::new(label)).clicked() {
                                            close = true;
                                        }
                                    });
                                });
                        }
                    }
                });

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
    let (step, failed) = match phase {
        FlashPhase::Resetting | FlashPhase::WaitingForReset => (0, false),
        FlashPhase::WritingStub { .. } => (1, false),
        FlashPhase::VerifyingStub { .. } => (2, false),
        FlashPhase::BootingStub | FlashPhase::StubBooted => (3, false),
        FlashPhase::Erasing => (4, false),
        FlashPhase::Programming { .. } => (5, false),
        FlashPhase::Verifying { .. } => (6, false),
        FlashPhase::Done => (7, false),
        FlashPhase::Failed { at_step, .. } => (*at_step, true),
    };

    let step0_label = match phase {
        FlashPhase::WaitingForReset => "Waiting for device reset...",
        _ => "Resetting device...",
    };

    let labels = [
        step0_label,
        "Writing stub to RAM",
        "Verifying stub",
        "Booting stub...",
        "Erasing flash",
        "Programming firmware",
        "Verifying firmware",
    ];

    for (i, label) in labels.iter().enumerate() {
        if i < step {
            ui.horizontal(|ui| {
                ui.colored_label(theme::INFO, icon::CHECK_CIRCLE);
                ui.colored_label(theme::INFO, *label);
            });
        } else if i == step && failed {
            ui.horizontal(|ui| {
                ui.colored_label(theme::ERROR, icon::X_CIRCLE);
                ui.colored_label(theme::ERROR, *label);
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

        if i == 0 && i == step && matches!(phase, FlashPhase::WaitingForReset) {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.colored_label(
                    theme::ERROR,
                    "Couldn't auto-reset — please reset your device.",
                );
            });
            ui.add_space(2.0);
        }

        // Progress bars for steps that report byte counts.
        let progress = match (i, phase) {
            (1, FlashPhase::WritingStub { bytes_written, bytes_total }) => {
                Some((*bytes_written, *bytes_total))
            }
            (2, FlashPhase::VerifyingStub { bytes_verified, bytes_total }) => {
                Some((*bytes_verified, *bytes_total))
            }
            (5, FlashPhase::Programming { bytes_written, bytes_total }) => {
                Some((*bytes_written, *bytes_total))
            }
            (6, FlashPhase::Verifying { bytes_verified, bytes_total }) => {
                Some((*bytes_verified, *bytes_total))
            }
            _ => None,
        };
        if let Some((done, total)) = progress {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                let fraction = if total > 0 {
                    done as f32 / total as f32
                } else {
                    0.0
                };
                let bar = egui::ProgressBar::new(fraction).text(format!(
                    "{:.1} / {:.1} KB",
                    done as f32 / 1024.0,
                    total as f32 / 1024.0,
                ));
                ui.add(bar);
            });
            ui.add_space(2.0);
        }
    }

    match phase {
        FlashPhase::Done => {
            ui.add_space(4.0);
            ui.colored_label(theme::INFO, "Complete!");
        }
        FlashPhase::Failed { message, .. } => {
            ui.add_space(4.0);
            ui.colored_label(theme::ERROR, message);
        }
        _ => {}
    }
}
