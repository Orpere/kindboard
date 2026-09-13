//! Small shared UI helpers: status pills, monogram colors, formatting.
//! No core logic lives here — presentation only.

use chrono::{DateTime, Utc};
use eframe::egui::{self, Color32, CornerRadius, RichText, Ui, Vec2};
use kindboard_core::{PodPhase, ToolId};

use crate::theme;

/// Draw a rounded "pill" label: colored dot + text on a dim background of
/// the same hue.
pub fn pill(ui: &mut Ui, text: impl Into<String>, color: Color32) {
    let dim = theme::pal().dim(color);
    let padding = egui::vec2(8.0, 3.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.into(), egui::FontId::proportional(12.0), color);
    let size = galley.size() + padding * 2.0;
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(9),
        dim,
        egui::Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
    let dot_center = egui::pos2(rect.left() + 6.0, rect.center().y);
    painter.circle_filled(dot_center, 3.0, color);
    painter.galley(
        egui::pos2(rect.left() + 12.0, rect.top() + padding.y),
        galley,
        color,
    );
}

/// A small round status dot (no text).
pub fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

/// Text color guaranteed legible on a given opaque fill in every theme
/// (R8): black on bright fills, white on dark fills, by relative luminance
/// (ITU-R BT.709). Pair this with every explicitly filled button.
#[must_use]
pub fn on_fill(color: Color32) -> Color32 {
    let luminance = 0.2126 * f32::from(color.r())
        + 0.7152 * f32::from(color.g())
        + 0.0722 * f32::from(color.b());
    if luminance > 150.0 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

/// Status color of a pod phase (contracts §5 palette).
pub fn pod_phase_color(phase: PodPhase) -> Color32 {
    match phase {
        PodPhase::Running => theme::pal().green,
        PodPhase::Pending => theme::pal().amber,
        PodPhase::Succeeded => theme::pal().succeeded,
        PodPhase::Failed => theme::pal().red,
        PodPhase::Unknown => theme::pal().grey,
    }
}

/// Two-letter monogram for a tool row icon (PNG tiles arrive in the docs
/// phase; monograms are the graceful fallback).
pub fn tool_monogram(id: ToolId) -> &'static str {
    match id {
        ToolId::Docker => "DK",
        ToolId::Kind => "KD",
        ToolId::Kubectl => "KC",
        ToolId::Helm => "HM",
        ToolId::Cilium => "CL",
        ToolId::K9s => "K9",
        ToolId::Kubectx => "KX",
        ToolId::Kustomize => "KZ",
    }
}

/// Brand-ish color per tool.
pub fn tool_color(id: ToolId) -> Color32 {
    match id {
        ToolId::Docker => Color32::from_rgb(0x24, 0x96, 0xed),
        ToolId::Kind => Color32::from_rgb(0x34, 0xc2, 0x9e),
        ToolId::Kubectl => Color32::from_rgb(0x32, 0x6c, 0xe5),
        ToolId::Helm => Color32::from_rgb(0x6b, 0x76, 0xd9),
        ToolId::Cilium => Color32::from_rgb(0xf2, 0xb8, 0x2e),
        ToolId::K9s => Color32::from_rgb(0xe8, 0x7a, 0x3e),
        ToolId::Kubectx => Color32::from_rgb(0x9b, 0x6c, 0xd9),
        ToolId::Kustomize => Color32::from_rgb(0x2f, 0xb8, 0xc9),
    }
}

/// `HH:MM:SS` of a k8s event timestamp.
pub fn time_of(timestamp: &DateTime<Utc>) -> String {
    timestamp.format("%H:%M:%S").to_string()
}

/// Truncate a string for display with an ASCII ellipsis.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max.saturating_sub(3)).collect();
        format!("{cut}...")
    }
}

/// Show an inline error line in red (form validation etc).
pub fn inline_error(ui: &mut Ui, message: &str) {
    ui.add_space(2.0);
    ui.label(
        RichText::new(format!("! {message}"))
            .color(theme::pal().red)
            .size(12.0),
    );
    ui.add_space(2.0);
}

/// Show an inline success/notice line.
pub fn inline_note(ui: &mut Ui, message: &str) {
    ui.add_space(2.0);
    ui.label(
        RichText::new(message)
            .color(theme::pal().text_dim)
            .size(12.0),
    );
    ui.add_space(2.0);
}
