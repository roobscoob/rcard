mod app;
mod bridge;
mod panels;
mod port_registry;
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

            // egui_taffy prerequisites (see its README):
            //
            // 1. Multi-pass layout is required so taffy-measured leaves
            //    can settle across passes. Without this, first-frame sizes
            //    are wrong (often zero-width) and text wraps at every
            //    space, growing vertically.
            // 2. Default text wrap mode must be Extend so labels inside
            //    taffy leaves compute their natural (single-line) width.
            //    With Wrap, egui labels ask for the smallest possible
            //    width, which collapses taffy flex children to zero.
            ctx.options_mut(|o| {
                o.max_passes = std::num::NonZeroUsize::new(3).unwrap();
            });
            ctx.style_mut(|s| {
                s.wrap_mode = Some(egui::TextWrapMode::Extend);
            });

            // Spawn the bridge before moving the runtime into the app.
            let handle = runtime.handle().clone();
            handle.spawn(bridge::run(cmd_rx, event_tx, ctx.clone()));

            Ok(Box::new(app::RcardApp::new(cmd_tx, event_rx, ctx, runtime)))
        }),
    )
}
