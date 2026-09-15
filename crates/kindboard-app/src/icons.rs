//! Asset loading (window icon) and tool-logo painting.
//!
//! Tool logos are the official project logos (see assets/ATTRIBUTION.md),
//! embedded at compile time. kubectl uses the Kubernetes logo (it is part
//! of Kubernetes); kubectx has no official logo and falls back to a
//! colored monogram box. Asset loading tolerates missing files: it logs
//! once and continues — never a crash.

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

/// Official tool logo PNGs (64 px), embedded at compile time. `None` for
/// tools without any official logo (kubectx) — the monogram fallback is
/// drawn instead. kubectl is part of Kubernetes, so it uses the Kubernetes
/// logo.
fn tool_logo_png(id: ToolId) -> Option<&'static [u8]> {
    Some(match id {
        ToolId::Docker => include_bytes!("../../../assets/logos/docker-64.png"),
        ToolId::Kind => include_bytes!("../../../assets/logos/kind-64.png"),
        ToolId::Kubectl => include_bytes!("../../../assets/logos/kubernetes-64.png"),
        ToolId::Helm => include_bytes!("../../../assets/logos/helm-64.png"),
        ToolId::Cilium => include_bytes!("../../../assets/logos/cilium-64.png"),
        ToolId::K9s => include_bytes!("../../../assets/logos/k9s-64.png"),
        ToolId::Kubectx => return None,
        ToolId::Kustomize => include_bytes!("../../../assets/logos/kustomize-64.png"),
    })
}

/// Decode an embedded PNG and load it as an egui texture (cached by name).
fn load_texture(ctx: &egui::Context, name: &str, png: &[u8]) -> Option<egui::TextureHandle> {
    let image = image::load_from_memory(png).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
}

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

/// Draw the official logo for a tool (kubectl uses the Kubernetes logo).
/// Returns `true` when a logo was drawn, `false` when the tool has none
/// (caller falls back to a monogram).
pub fn logo_image(ui: &mut Ui, id: ToolId, size: f32) -> bool {
    let Some(png) = tool_logo_png(id) else {
        return false;
    };
    let Some(texture) = load_texture(ui.ctx(), &format!("tool-logo-{id}"), png) else {
        return false;
    };
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    ui.painter().image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
        // Theme-aware tint (ADR-0024): white lettering would vanish on light
        // backgrounds; the palette picks a dark tint for light themes.
        crate::theme::pal().logo_tint,
    );
    true
}

/// Draw a tool icon: the official logo when one exists (kubectl uses the
/// Kubernetes logo), otherwise a colored rounded square with a 2-letter
/// monogram (kubectx has no official logo).
pub fn tool_icon(ui: &mut Ui, id: ToolId, size: f32) {
    if logo_image(ui, id, size) {
        return;
    }
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

/// Small brand logo box for the window chrome/headers (the kindboard mark,
/// embedded at compile time).
pub fn brand_mark(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    if let Some(texture) = load_texture(
        ui.ctx(),
        "brand-mark",
        include_bytes!("../../../assets/icons/kindboard-64.png"),
    ) {
        ui.painter().image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            // Theme-aware tint (ADR-0024), same rule as the tool logos.
            crate::theme::pal().logo_tint,
        );
        return;
    }
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(6),
        crate::theme::pal().accent,
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
