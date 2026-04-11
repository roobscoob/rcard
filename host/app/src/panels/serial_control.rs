use device::logs::ControlEvent;
use egui_extras::{Column, TableBuilder};
use rcard_usb_proto::messages::TunnelErrorCode;

use crate::theme;
use egui_phosphor::regular as icon;

const ROW_HEIGHT: f32 = 20.0;

/// Render decoded IPC control events from a USART2 adapter.
///
/// Three kinds of rows: tunnel errors (red), IPC replies (blue), and
/// unknown / decode failures (dim). Each row shows a kind chip, frame
/// sequence number, and a kind-specific detail string.
pub fn show(ui: &mut egui::Ui, events: &[ControlEvent]) {
    if events.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 2.0 - 30.0);
                ui.colored_label(
                    theme::TEXT_DIM,
                    egui::RichText::new(icon::TREE_STRUCTURE).size(32.0),
                );
                ui.colored_label(theme::TEXT_DIM, "Waiting for IPC traffic...");
            });
        });
        return;
    }

    let table = TableBuilder::new(ui)
        .striped(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .stick_to_bottom(true)
        .column(Column::exact(110.0)) // kind
        .column(Column::exact(60.0))  // seq
        .column(Column::remainder()); // detail

    table
        .header(ROW_HEIGHT, |mut header| {
            header.col(|ui| {
                ui.colored_label(theme::TEXT_SECONDARY, "Kind");
            });
            header.col(|ui| {
                ui.colored_label(theme::TEXT_SECONDARY, "Seq");
            });
            header.col(|ui| {
                ui.colored_label(theme::TEXT_SECONDARY, "Detail");
            });
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, events.len(), |mut row| {
                let event = &events[row.index()];
                let (color, kind_label, seq_text, detail) = row_content(event);

                row.col(|ui| {
                    ui.colored_label(color, kind_label);
                });
                row.col(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(0x80, 0x87, 0xa2),
                        seq_text,
                    );
                });
                row.col(|ui| {
                    ui.label(detail);
                });
            });
        });
}

fn row_content(event: &ControlEvent) -> (egui::Color32, String, String, String) {
    match event {
        ControlEvent::Awake {
            seq,
            uid,
            firmware_id,
        } => (
            theme::INFO,
            format!("{} AWAKE", icon::SUN),
            format!("{seq}"),
            format!("uid={} fw={}", hex16(uid), hex16(firmware_id)),
        ),
        ControlEvent::TunnelError { code, seq } => (
            theme::ERROR,
            format!("{} TUN-ERR", icon::WARNING_CIRCLE),
            format!("{seq}"),
            tunnel_error_label(*code).to_string(),
        ),
        ControlEvent::IpcReply { seq, payload } => (
            theme::INFO,
            format!("{} IPC-REPLY", icon::ARROW_LEFT),
            format!("{seq}"),
            format!("{} bytes", payload.len()),
        ),
        ControlEvent::UnknownSimple {
            seq,
            opcode,
            payload,
        } => (
            theme::TEXT_DIM,
            format!("{} SIMPLE", icon::QUESTION),
            format!("{seq}"),
            format!("op=0x{opcode:02x} len={}", payload.len()),
        ),
        ControlEvent::FrameError(msg) => (
            theme::ERROR,
            format!("{} FRAME-ERR", icon::X_CIRCLE),
            String::from("—"),
            msg.clone(),
        ),
    }
}

fn hex16(data: &[u8; 16]) -> String {
    let mut s = String::with_capacity(32);
    for b in data {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn tunnel_error_label(code: TunnelErrorCode) -> &'static str {
    match code {
        TunnelErrorCode::TaskDead => "TaskDead — target task dead or restarted",
        TunnelErrorCode::LeasePoolFull => "LeasePoolFull — leases exceed 8K pool",
        TunnelErrorCode::BadRequest => "BadRequest — malformed request frame",
        TunnelErrorCode::NoHostForwarding => {
            "NoHostForwarding — firmware lacks sysmodule_host_proxy"
        }
        TunnelErrorCode::Busy => "Busy — transport already has a pending request",
        TunnelErrorCode::Internal => "Internal — unspecified tunnel error",
    }
}
