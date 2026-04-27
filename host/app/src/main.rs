mod app;
mod bridge;
mod ipc;
mod ipc_handle;
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

            let mut fonts = egui::FontDefinitions::default();
            egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);

            if let Some(cjk) = load_cjk_fallback() {
                fonts.font_data.insert("cjk_fallback".into(), cjk.into());
                for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
                    if let Some(list) = fonts.families.get_mut(&family) {
                        list.push("cjk_fallback".into());
                    }
                }
            }

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
            // Bridge needs a `cmd_tx` clone too so it can self-enqueue
            // commands (e.g. the batched `ProbeMoshiMoshi` fired by
            // `usart1_connect_loop` when the USART1 settling set empties).
            let handle = runtime.handle().clone();
            handle.spawn(bridge::run(cmd_rx, cmd_tx.clone(), event_tx, ctx.clone()));

            Ok(Box::new(app::RcardApp::new(cmd_tx, event_rx, ctx, runtime)))
        }),
    )
}

fn load_cjk_fallback() -> Option<egui::FontData> {
    use font_kit::family_name::FamilyName;
    use font_kit::properties::Properties;
    use font_kit::source::SystemSource;

    let source = SystemSource::new();
    let handle = source
        .select_best_match(
            &[
                FamilyName::Title("Noto Sans CJK SC".into()),
                FamilyName::Title("Microsoft YaHei".into()),
                FamilyName::Title("Hiragino Sans".into()),
                FamilyName::SansSerif,
            ],
            &Properties::new(),
        )
        .ok()?;
    let font = handle.load().ok()?;
    let data = font.copy_font_data()?.to_vec();
    Some(egui::FontData::from_owned(data))
}
