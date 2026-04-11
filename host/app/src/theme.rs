use egui::{Color32, CornerRadius, Stroke, Style, Visuals, style::Spacing};

// ── Palette ────────────────────────────────────────────────────────────
const BG: Color32 = Color32::from_rgb(0x1B, 0x1D, 0x2B);
const PANEL: Color32 = Color32::from_rgb(0x23, 0x25, 0x36);
const SIDEBAR: Color32 = Color32::from_rgb(0x1E, 0x20, 0x30);
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xCA, 0xD3, 0xF5);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0x80, 0x87, 0xA2);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x60, 0x65, 0x80);
pub const ACCENT: Color32 = Color32::from_rgb(0x8A, 0xAD, 0xF4);
const WIDGET_BG: Color32 = Color32::from_rgb(0x2A, 0x2D, 0x42);
const WIDGET_HOVERED: Color32 = Color32::from_rgb(0x33, 0x36, 0x50);
const WIDGET_ACTIVE: Color32 = Color32::from_rgb(0x3D, 0x40, 0x5C);

// Semantic colors.
pub const ERROR: Color32 = Color32::from_rgb(0xED, 0x87, 0x96);
pub const WARN: Color32 = Color32::from_rgb(0xEE, 0xD4, 0x9F);
pub const INFO: Color32 = Color32::from_rgb(0xA6, 0xDA, 0x95);
pub const DEBUG: Color32 = Color32::from_rgb(0x8A, 0xAD, 0xF4);
pub const TRACE: Color32 = Color32::from_rgb(0x60, 0x65, 0x80);

// Surface colors for cards/sections.
#[allow(dead_code)]
pub const SURFACE: Color32 = Color32::from_rgb(0x2A, 0x2D, 0x42);
#[allow(dead_code)]
pub const SURFACE_BORDER: Color32 = Color32::from_rgb(0x33, 0x36, 0x50);

pub fn apply(ctx: &egui::Context) {
    let mut style = Style::default();

    // Spacing.
    style.spacing = Spacing {
        item_spacing: egui::vec2(8.0, 4.0),
        button_padding: egui::vec2(8.0, 4.0),
        indent: 16.0,
        ..style.spacing
    };

    // Visuals.
    let mut visuals = Visuals::dark();

    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = BG;
    visuals.faint_bg_color = SIDEBAR;

    visuals.override_text_color = Some(TEXT_PRIMARY);
    visuals.selection.bg_fill = ACCENT.linear_multiply(0.3);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;

    // Widget styles.
    let rounding = CornerRadius::same(4);

    visuals.widgets.noninteractive.bg_fill = WIDGET_BG;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
    visuals.widgets.noninteractive.corner_radius = rounding;

    visuals.widgets.inactive.bg_fill = WIDGET_BG;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.inactive.corner_radius = rounding;

    visuals.widgets.hovered.bg_fill = WIDGET_HOVERED;
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.hovered.corner_radius = rounding;

    visuals.widgets.active.bg_fill = WIDGET_ACTIVE;
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.active.corner_radius = rounding;

    visuals.widgets.open.bg_fill = WIDGET_ACTIVE;
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.open.corner_radius = rounding;

    // Window.
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.window_stroke = Stroke::new(1.0, Color32::from_rgb(0x30, 0x33, 0x4A));

    style.visuals = visuals;
    ctx.set_style(style);
}
