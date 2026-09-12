//! Asset loading (window icon) and icon-ish widget painting.
//!
//! The docs phase will add PNG logo tiles under `assets/icons/`; until then
//! the app falls back to monogram boxes. Asset loading tolerates missing
//! files: it logs once and continues — never a crash.

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Ui, Vec2};
use kindboard_core::ToolId;

use crate::util::{tool_color, tool_monogram};

/// Candidate paths for the window icon, tried in order. The compile-time
/// path anchors the repo layout; the relative ones work from `target/` and
/// install dirs.
const ICON_CANDIDATES: &[&str] = &[
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/icons/kindboard-64.png"
    ),
    "assets/icons/kindboard-64.png",
    "../assets/icons/kindboard-64.png",
];

/// Load the window icon (`assets/icons/kindboard-64.png`) if present.
/// Logs once when absent; returns `None` — the OS default icon is used.
pub fn load_window_icon() -> Option<Arc<egui::viewport::IconData>> {
    for path in ICON_CANDIDATES {
        match std::fs::read(path) {
            Ok(bytes) => match decode_icon(&bytes) {
                Ok(icon) => return Some(Arc::new(icon)),
                Err(err) => log::warn!("kindboard: icon at {path} failed to decode: {err}"),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => log::warn!("kindboard: cannot read icon at {path}: {err}"),
        }
    }
    log::warn!(
        "kindboard: no window icon found (expected assets/icons/kindboard-64.png); using the OS default icon"
    );
    None
}

/// Decode PNG bytes into an [`egui::viewport::IconData`] (RGBA8).
fn decode_icon(bytes: &[u8]) -> Result<egui::viewport::IconData, String> {
    let image = image::load_from_memory(bytes)
        .map_err(|err| err.to_string())?
        .to_rgba8();
    let (width, height) = (image.width(), image.height());
    Ok(egui::viewport::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

/// Draw a tool icon: a colored rounded square with a 2-letter monogram.
/// This is the fallback until real logo PNGs land; the tile slot is kept
/// so swapping to `Image` later is a one-line change.
pub fn tool_icon(ui: &mut Ui, id: ToolId, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let color = tool_color(id);
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(5),
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 46),
        egui::Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        tool_monogram(id),
        egui::FontId::proportional(size * 0.38),
        color,
    );
}

/// Small brand logo box for the window chrome/headers (monogram fallback).
pub fn brand_mark(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(6),
        crate::theme::ACCENT,
        egui::Stroke::NONE,
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "kb",
        egui::FontId::proportional(size * 0.45),
        Color32::from_rgb(0x0e, 0x16, 0x1e),
    );
}
