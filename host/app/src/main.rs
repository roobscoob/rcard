mod app;
mod bridge;
mod panels;
mod sidebar;
mod state;
mod stub;
mod theme;

fn main() -> eframe::Result<()> {
    let runtime = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (event_tx, event_rx) = crossbeam_channel::unbounded();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("rcard")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };

    eframe::run_native(
        "rcard",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            theme::apply(&ctx);

            // Load Phosphor icon font into egui's proportional family.
            let mut fonts = egui::FontDefinitions::default();
            egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
            ctx.set_fonts(fonts);

            // Spawn the bridge before moving the runtime into the app.
            let handle = runtime.handle().clone();
            handle.spawn(bridge::run(cmd_rx, event_tx, ctx.clone()));

            Ok(Box::new(app::RcardApp::new(cmd_tx, event_rx, ctx, runtime)))
        }),
    )
}
