//! Asset loading (window icon) and tool-logo painting.
//!
//! All artwork is embedded at compile time: the window icon is the ORP
//! mark (`assets/icons/orp-mark-64.png`) and the tool logos are the
//! official project logos (see assets/ATTRIBUTION.md). kubectl uses the
//! Kubernetes logo (it is part of Kubernetes); kubectx has no official
//! logo and falls back to a colored monogram box. Decode failures log
//! once and continue — never a crash.

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Ui, Vec2};
use kindboard_core::ToolId;

use crate::util::{tool_color, tool_monogram};

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

/// Light-theme recolor for artwork that is white/very-light lettering
/// (ADR-0025). Painted with the identity tint, those pixels would vanish
/// on pale surfaces — so at texture load, near-opaque pixels (alpha > 200)
/// brighter than `max_light_lum` (WCAG relative luminance, linear-light)
/// are replaced by `target` (alpha untouched; anti-aliased edges keep
/// blending against the background).
#[derive(Debug, Clone, Copy)]
pub(crate) struct LightRecolor {
    pub(crate) max_light_lum: f32,
    pub(crate) target: [u8; 3],
}

/// Recolor target shared by all light-theme recolors: the light theme's
/// dark neutral (same family as its text) — ~10.8:1 on `bg`.
pub(crate) const LIGHT_RECOLOR_TARGET: [u8; 3] = [0x2f, 0x3a, 0x45];

/// kind wordmark: light-blue letters (lum ≈ 0.72) — threshold sits below
/// them and above the navy details and the orange accent (lum ≈ 0.20).
const KIND_MAX_LIGHT_LUM: f32 = 0.45;

/// cilium: white hexagon lattice (lum ≈ 1.0) — threshold sits above all
/// colored accents (lum ≤ 0.65), so only the whites are recolored.
const CILIUM_MAX_LIGHT_LUM: f32 = 0.80;

/// The light-theme recolor for a tool logo, if its artwork needs one.
fn tool_logo_light_recolor(id: ToolId) -> Option<LightRecolor> {
    Some(match id {
        ToolId::Kind => LightRecolor {
            max_light_lum: KIND_MAX_LIGHT_LUM,
            target: LIGHT_RECOLOR_TARGET,
        },
        ToolId::Cilium => LightRecolor {
            max_light_lum: CILIUM_MAX_LIGHT_LUM,
            target: LIGHT_RECOLOR_TARGET,
        },
        _ => return None,
    })
}

/// WCAG relative luminance of an sRGB byte triplet (linear-light).
fn luminance(r: u8, g: u8, b: u8) -> f32 {
    let channel = |v: u8| {
        let s = f32::from(v) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// Replace fully opaque pixels brighter than `recolor.max_light_lum` with
/// `recolor.target` (alpha preserved). Returns the number of recolored
/// pixels.
pub(crate) fn recolor_light_pixels(rgba: &mut [u8], recolor: LightRecolor) -> usize {
    // RGBA rows are always a multiple of 4; the remainder is empty.
    debug_assert_eq!(
        rgba.len() % 4,
        0,
        "RGBA buffer length must be a multiple of 4"
    );
    let mut recolored = 0;
    for px in rgba.as_chunks_mut::<4>().0 {
        if px[3] <= 200 {
            continue; // translucent edge: keep the original color (AA)
        }
        if luminance(px[0], px[1], px[2]) > recolor.max_light_lum {
            px[0] = recolor.target[0];
            px[1] = recolor.target[1];
            px[2] = recolor.target[2];
            recolored += 1;
        }
    }
    recolored
}

/// Decode an embedded PNG and load it as an egui texture (cached by name),
/// optionally recoloring its light lettering for the Light theme.
fn load_texture(
    ctx: &egui::Context,
    name: &str,
    png: &[u8],
    light_recolor: Option<LightRecolor>,
) -> Option<egui::TextureHandle> {
    let image = image::load_from_memory(png).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let mut rgba = image.into_raw();
    if let Some(recolor) = light_recolor {
        recolor_light_pixels(&mut rgba, recolor);
    }
    let color = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
    Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
}

/// Load the window icon — the ORP mark (`assets/icons/orp-mark-64.png`),
/// embedded at compile time. Returns `None` only if the embedded bytes
/// fail to decode, in which case the OS default icon is used.
pub fn load_window_icon() -> Option<Arc<egui::viewport::IconData>> {
    const ORP_MARK_PNG: &[u8] = include_bytes!("../../../assets/icons/orp-mark-64.png");
    match decode_icon(ORP_MARK_PNG) {
        Ok(icon) => Some(Arc::new(icon)),
        Err(err) => {
            log::warn!("kindboard: embedded ORP mark icon failed to decode: {err}");
            None
        }
    }
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
    // ADR-0025: the Light theme paints logos in their original brand
    // colors (identity tint) and recolors white-lettering artwork at load
    // time; dark themes keep the historical white silhouette. Textures are
    // cached per theme so switching themes reloads the right pixels.
    let light = crate::theme::current() == crate::theme::ThemeId::Light;
    let recolor = if light {
        tool_logo_light_recolor(id)
    } else {
        None
    };
    let name = format!("tool-logo-{id}-{}", crate::theme::current().id());
    let Some(texture) = load_texture(ui.ctx(), &name, png, recolor) else {
        return false;
    };
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
    ui.painter().image(
        texture.id(),
        rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
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
    // Same light-theme treatment as the tool logos (ADR-0025): original
    // colors with the light-blue wordmark recolored dark at load time.
    let light = crate::theme::current() == crate::theme::ThemeId::Light;
    let recolor = light.then_some(LightRecolor {
        max_light_lum: KIND_MAX_LIGHT_LUM,
        target: LIGHT_RECOLOR_TARGET,
    });
    let name = format!("brand-mark-{}", crate::theme::current().id());
    if let Some(texture) = load_texture(
        ui.ctx(),
        &name,
        include_bytes!("../../../assets/icons/kindboard-64.png"),
        recolor,
    ) {
        ui.painter().image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG contrast ratio between an sRGB triplet and an opaque color.
    fn contrast(rgb: [u8; 3], other: [u8; 3]) -> f32 {
        let lum = |c: [u8; 3]| {
            let channel = |v: u8| {
                let s = f32::from(v) / 255.0;
                if s <= 0.04045 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
        };
        let (hi, lo) = {
            let (a, b) = (lum(rgb), lum(other));
            if a >= b { (a, b) } else { (b, a) }
        };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn recolor_recolors_bright_opaque_pixels_only() {
        // Near-opaque white → target; the alpha-201 pixel sits just above
        // the translucent-edge cutoff (≤ 200); opaque dark → untouched.
        let mut px = [
            0xff, 0xff, 0xff, 0xff, // opaque white → recolored
            0xff, 0xff, 0xff, 201, // alpha 201 → recolored (keeps alpha)
            0x11, 0x22, 0x33, 0xff, // dark → untouched
        ];
        let n = recolor_light_pixels(
            &mut px,
            LightRecolor {
                max_light_lum: 0.5,
                target: [0x2f, 0x3a, 0x45],
            },
        );
        assert_eq!(n, 2);
        assert_eq!(&px[0..4], &[0x2f, 0x3a, 0x45, 0xff]);
        assert_eq!(&px[4..8], &[0x2f, 0x3a, 0x45, 201]);
        assert_eq!(&px[8..12], &[0x11, 0x22, 0x33, 0xff]);
    }

    #[test]
    fn recolor_handles_empty_buffer() {
        let n = recolor_light_pixels(
            &mut [],
            LightRecolor {
                max_light_lum: 0.5,
                target: [0x2f, 0x3a, 0x45],
            },
        );
        assert_eq!(n, 0);
    }

    #[test]
    fn recolor_skips_translucent_edges() {
        // Anti-aliased edge pixels (alpha ≤ 200) keep their color so the
        // shape keeps blending against the background.
        let mut px = [0xff, 0xff, 0xff, 0x80, 0xff, 0xff, 0xff, 200];
        let n = recolor_light_pixels(
            &mut px,
            LightRecolor {
                max_light_lum: 0.0,
                target: [0x2f, 0x3a, 0x45],
            },
        );
        assert_eq!(n, 0);
        assert_eq!(px, [0xff, 0xff, 0xff, 0x80, 0xff, 0xff, 0xff, 200]);
    }

    #[test]
    fn kind_threshold_recolors_letters_keeps_accent() {
        // kind wordmark: light-blue letters (lum ≈ 0.72) above the
        // threshold; the orange accent (lum ≈ 0.20) below it.
        let blue = [0xbb, 0xe3, 0xf8, 0xff];
        let orange = [0xb8, 0x66, 0x3a, 0xff];
        let mut px = [blue, orange].concat();
        let n = recolor_light_pixels(
            &mut px,
            LightRecolor {
                max_light_lum: KIND_MAX_LIGHT_LUM,
                target: LIGHT_RECOLOR_TARGET,
            },
        );
        assert_eq!(n, 1);
        assert_eq!(&px[0..4], &[0x2f, 0x3a, 0x45, 0xff]);
        assert_eq!(&px[4..8], &orange);
    }

    #[test]
    fn cilium_threshold_recolors_whites_keeps_accents() {
        // cilium: the white lattice (lum ≈ 1.0) above the threshold; the
        // yellow accent (#f8c517, lum ≈ 0.60) below it.
        let white = [0xff, 0xff, 0xff, 0xff];
        let yellow = [0xf8, 0xc5, 0x17, 0xff];
        let mut px = [white, yellow].concat();
        let n = recolor_light_pixels(
            &mut px,
            LightRecolor {
                max_light_lum: CILIUM_MAX_LIGHT_LUM,
                target: LIGHT_RECOLOR_TARGET,
            },
        );
        assert_eq!(n, 1);
        assert_eq!(&px[0..4], &[0x2f, 0x3a, 0x45, 0xff]);
        assert_eq!(&px[4..8], &yellow);
    }

    #[test]
    fn only_kind_and_cilium_get_a_light_recolor() {
        // Every other logo renders in its original brand colors on the
        // Light theme (ADR-0025).
        assert!(tool_logo_light_recolor(ToolId::Kind).is_some());
        assert!(tool_logo_light_recolor(ToolId::Cilium).is_some());
        for id in [
            ToolId::Docker,
            ToolId::Kubectl,
            ToolId::Helm,
            ToolId::K9s,
            ToolId::Kubectx,
            ToolId::Kustomize,
        ] {
            assert!(
                tool_logo_light_recolor(id).is_none(),
                "{id} must render in original colors on Light"
            );
        }
    }

    #[test]
    fn light_recolor_target_contrasts_with_light_bg() {
        // Regression (ADR-0024 → ADR-0025): the recolored lettering must
        // clear 3:1 (large glyphs) on the Light theme's background.
        let pal = crate::theme::ThemeId::Light.palette();
        let ratio = contrast(LIGHT_RECOLOR_TARGET, [pal.bg.r(), pal.bg.g(), pal.bg.b()]);
        assert!(
            ratio >= 3.0,
            "recolor target {:?} on Light bg {:?} = {ratio:.2}:1 (need ≥ 3:1)",
            LIGHT_RECOLOR_TARGET,
            pal.bg,
        );
    }
}
