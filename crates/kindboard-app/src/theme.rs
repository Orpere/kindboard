//! Visual identity: dark theme, accent color, status palette, spacing.
//! Colors are chosen for contrast on the dark background (WCAG-ish):
//! status text is paired with a dim fill of the same hue.

use eframe::egui::{self, Color32, CornerRadius, Margin, Style, Visuals};

/// Brand accent (teal-blue).
pub const ACCENT: Color32 = Color32::from_rgb(0x3d, 0xa5, 0xd9);
/// Accent used as a hover fill.
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x2a, 0x76, 0x9d);
/// Text on the accent fill (high contrast).
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x06, 0x0d, 0x12);
/// Healthy/ready state.
pub const GREEN: Color32 = Color32::from_rgb(0x4d, 0xc0, 0x64);
/// Warning/pending state.
pub const AMBER: Color32 = Color32::from_rgb(0xf0, 0xa5, 0x2e);
/// Error/failed state.
pub const RED: Color32 = Color32::from_rgb(0xe5, 0x53, 0x4b);
/// Unknown/neutral state.
pub const GREY: Color32 = Color32::from_rgb(0x8a, 0x96, 0xa3);
/// Surface behind panels.
pub const BG: Color32 = Color32::from_rgb(0x12, 0x16, 0x1c);
/// Slightly raised surface.
pub const BG_RAISED: Color32 = Color32::from_rgb(0x1a, 0x20, 0x28);
/// Stroke around cards/nodes.
pub const STROKE: Color32 = Color32::from_rgb(0x2c, 0x36, 0x42);
/// Diagram canvas background.
pub const CANVAS_BG: Color32 = Color32::from_rgb(0x0e, 0x12, 0x17);
/// Text on dark surfaces.
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9a, 0xa6, 0xb4);

/// Dim (background) variant of a status color for pills and badges.
#[must_use]
pub fn dim(color: Color32) -> Color32 {
    let [r, g, b, _] = color.to_array();
    Color32::from_rgb(r / 5, g / 5, b / 5)
}

/// Apply the kindboard theme to an egui context.
pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = BG_RAISED;
    visuals.window_stroke = egui::Stroke::new(1.0, STROKE);
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, STROKE);
    visuals.widgets.inactive.bg_fill = BG_RAISED;
    visuals.widgets.inactive.weak_bg_fill = BG_RAISED;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, STROKE);
    visuals.widgets.hovered.bg_fill = ACCENT_HOVER;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.extreme_bg_color = Color32::from_rgb(0x0a, 0x0d, 0x11);
    ctx.set_visuals(visuals);

    let mut style: Style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.window_margin = Margin::same(14);
    style.spacing.scroll = egui::style::ScrollStyle::solid();
    ctx.set_global_style(style);
}

/// Standard frame for a card/section.
pub fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(BG_RAISED)
        .stroke(egui::Stroke::new(1.0, STROKE))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(10))
}

/// Frame for the diagram canvas (darker, inset feel).
pub fn canvas_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(CANVAS_BG)
        .stroke(egui::Stroke::new(1.0, STROKE))
        .corner_radius(CornerRadius::same(6))
}
