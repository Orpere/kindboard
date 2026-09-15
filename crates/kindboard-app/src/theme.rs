//! Visual identity: runtime-selectable themes (dark / light /
//! high-contrast). The design language (cards, strokes, status palette,
//! spacing) is identical across themes — only the palette changes.
//!
//! The active palette is a `const`-constructed array behind an `AtomicU8`
//! index: `pal()` is lock-free and allocation-free (per-frame hot path),
//! and the index can only ever hold a valid `ThemeId`, so it cannot be
//! poisoned or go out of bounds.

use eframe::egui::{self, Color32, Margin, Style, Visuals};
use std::sync::atomic::{AtomicU8, Ordering};

/// One of the three selectable themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeId {
    /// Default: the original kindboard dark identity.
    Dark,
    /// Light: pale surfaces, darker accents (same language).
    Light,
    /// High contrast: pure black surfaces, white strokes.
    HighContrast,
}

impl ThemeId {
    /// All themes in UI order.
    pub const ALL: [ThemeId; 3] = [ThemeId::Dark, ThemeId::Light, ThemeId::HighContrast];

    /// Default theme (also the fallback for missing/invalid settings).
    pub const DEFAULT: ThemeId = ThemeId::Dark;

    /// Stable persistence id: `"dark" | "light" | "high-contrast"`.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            ThemeId::Dark => "dark",
            ThemeId::Light => "light",
            ThemeId::HighContrast => "high-contrast",
        }
    }

    /// UI label: `"Dark" | "Light" | "High Contrast"`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ThemeId::Dark => "Dark",
            ThemeId::Light => "Light",
            ThemeId::HighContrast => "High Contrast",
        }
    }

    /// Parse a persisted id (case-insensitive); `None` for unknown ids.
    #[must_use]
    pub fn from_id(id: &str) -> Option<ThemeId> {
        match id {
            _ if id.eq_ignore_ascii_case("dark") => Some(ThemeId::Dark),
            _ if id.eq_ignore_ascii_case("light") => Some(ThemeId::Light),
            _ if id.eq_ignore_ascii_case("high-contrast") => Some(ThemeId::HighContrast),
            _ => None,
        }
    }

    /// `true` only for [`ThemeId::Dark`].
    #[must_use]
    pub fn is_dark(self) -> bool {
        matches!(self, ThemeId::Dark)
    }

    /// The palette for this theme.
    #[must_use]
    pub fn palette(self) -> &'static Palette {
        &PALETTES[self as usize]
    }
}

/// A full visual palette. `Copy`, field-for-field `const`-constructible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    // --- brand/status (all themes) ---
    /// Brand accent (teal-blue).
    pub accent: Color32,
    /// Accent used as the pressed/active fill (darker than `accent` so
    /// `on_accent` text keeps ≥4.5:1 on it — R8).
    pub accent_hover: Color32,
    /// Text on the accent fill (always legible per R8).
    pub on_accent: Color32,
    /// Healthy/ready state.
    pub green: Color32,
    /// Warning/pending state.
    pub amber: Color32,
    /// Error/failed state.
    pub red: Color32,
    /// Unknown/neutral state.
    pub grey: Color32,
    /// Succeeded pod phase (was the inline `0x4f9dc9`).
    pub succeeded: Color32,
    // --- surfaces/structure ---
    /// Surface behind panels.
    pub bg: Color32,
    /// Slightly raised surface.
    pub bg_raised: Color32,
    /// Stroke around cards/nodes.
    pub stroke: Color32,
    /// Diagram canvas background.
    pub canvas_bg: Color32,
    /// Secondary text on surfaces.
    pub text_dim: Color32,
    /// Extreme background (text edits, scrollbars).
    pub extreme_bg: Color32,
    // --- hover (ADR-0022) ---
    /// Fill behind hovered widgets. Buttons/selectable labels paint this
    /// via egui's `widgets.hovered.weak_bg_fill`, which must match the
    /// hover text below (egui paints button fills from `weak_bg_fill`,
    /// not `bg_fill`).
    pub hover_fill: Color32,
    /// Text of hovered widgets: a gray readable on `hover_fill` in every
    /// theme (never the accent-on color, which turns white on light
    /// themes — ADR-0022).
    pub hover_text: Color32,
    // --- diagram-only colors (promoted from hardcoded values) ---
    /// Diagram node label text (was `0xd6e4ee`).
    pub node_text: Color32,
    /// Diagram tooltip background (was `from_black_alpha(220)`).
    pub tooltip_bg: Color32,
    /// Diagram tooltip text (was `WHITE`).
    pub tooltip_text: Color32,
    /// Diagram service fill (was `rgba(0x5b8db8, 40)`).
    pub service_fill: Color32,
    /// Diagram ingress fill (was `rgba(0x8d7ab8, 40)`).
    pub ingress_fill: Color32,
    // --- per-theme dim behavior ---
    /// `0.0` → darken path; `> 0` → lighten toward white by this amount.
    pub dim_lighten: f32,
    /// Byte-wise divisor for the darken path (unused when lightening).
    pub dim_divisor: u8,
}

impl Palette {
    /// Background "dim" variant of a status color for pills/badges/error
    /// strips.
    ///
    /// - Dark: `c / 5` (byte-wise) — the historical behavior.
    /// - Light: lighten toward white, `c + (255 - c) * 0.80` (rounded).
    /// - HighContrast: `c / 2` (byte-wise) — half-brightness hue, visible
    ///   on black with the white stroke.
    #[must_use]
    pub fn dim(&self, color: Color32) -> Color32 {
        if self.dim_lighten > 0.0 {
            let k = self.dim_lighten;
            let mix = |c: u8| (f32::from(c) + (255.0 - f32::from(c)) * k).round() as u8;
            Color32::from_rgb(mix(color.r()), mix(color.g()), mix(color.b()))
        } else {
            let d = self.dim_divisor.max(1);
            Color32::from_rgb(color.r() / d, color.g() / d, color.b() / d)
        }
    }
}

// --- palettes ---------------------------------------------------------------

const fn dark() -> Palette {
    Palette {
        accent: Color32::from_rgb(0x3d, 0xa5, 0xd9),
        accent_hover: Color32::from_rgb(0x3d, 0x86, 0xad),
        on_accent: Color32::from_rgb(0x06, 0x0d, 0x12),
        green: Color32::from_rgb(0x4d, 0xc0, 0x64),
        amber: Color32::from_rgb(0xf0, 0xa5, 0x2e),
        red: Color32::from_rgb(0xe5, 0x53, 0x4b),
        grey: Color32::from_rgb(0x8a, 0x96, 0xa3),
        succeeded: Color32::from_rgb(0x4f, 0x9d, 0xc9),
        bg: Color32::from_rgb(0x12, 0x16, 0x1c),
        bg_raised: Color32::from_rgb(0x1a, 0x20, 0x28),
        stroke: Color32::from_rgb(0x2c, 0x36, 0x42),
        canvas_bg: Color32::from_rgb(0x0e, 0x12, 0x17),
        text_dim: Color32::from_rgb(0x9a, 0xa6, 0xb4),
        extreme_bg: Color32::from_rgb(0x0a, 0x0d, 0x11),
        hover_fill: Color32::from_rgb(0x27, 0x32, 0x3d),
        hover_text: Color32::from_rgb(0xc6, 0xcf, 0xd9),
        node_text: Color32::from_rgb(0xd6, 0xe4, 0xee),
        tooltip_bg: Color32::from_black_alpha(220),
        tooltip_text: Color32::WHITE,
        service_fill: Color32::from_rgba_unmultiplied_const(0x5b, 0x8d, 0xb8, 40),
        ingress_fill: Color32::from_rgba_unmultiplied_const(0x8d, 0x7a, 0xb8, 40),
        dim_lighten: 0.0,
        dim_divisor: 5,
    }
}

const fn light() -> Palette {
    Palette {
        accent: Color32::from_rgb(0x1b, 0x7f, 0xb2),
        accent_hover: Color32::from_rgb(0x18, 0x6c, 0x97),
        on_accent: Color32::from_rgb(0xff, 0xff, 0xff),
        green: Color32::from_rgb(0x1f, 0x8a, 0x3d),
        amber: Color32::from_rgb(0xb3, 0x6b, 0x00),
        red: Color32::from_rgb(0xc0, 0x39, 0x2b),
        grey: Color32::from_rgb(0x6b, 0x76, 0x83),
        succeeded: Color32::from_rgb(0x2b, 0x7e, 0xa3),
        bg: Color32::from_rgb(0xf5, 0xf7, 0xfa),
        bg_raised: Color32::from_rgb(0xff, 0xff, 0xff),
        stroke: Color32::from_rgb(0xd0, 0xd7, 0xdf),
        canvas_bg: Color32::from_rgb(0xee, 0xf1, 0xf5),
        text_dim: Color32::from_rgb(0x5a, 0x66, 0x72),
        extreme_bg: Color32::from_rgb(0xe6, 0xea, 0xf0),
        hover_fill: Color32::from_rgb(0xdc, 0xe3, 0xea),
        hover_text: Color32::from_rgb(0x37, 0x42, 0x4d),
        node_text: Color32::from_rgb(0x2b, 0x33, 0x3b),
        tooltip_bg: Color32::from_black_alpha(230),
        tooltip_text: Color32::WHITE,
        service_fill: Color32::from_rgba_unmultiplied_const(0x5b, 0x8d, 0xb8, 36),
        ingress_fill: Color32::from_rgba_unmultiplied_const(0x8d, 0x7a, 0xb8, 36),
        dim_lighten: 0.80,
        dim_divisor: 5,
    }
}

const fn high_contrast() -> Palette {
    Palette {
        accent: Color32::from_rgb(0x1f, 0x9b, 0xd6),
        accent_hover: Color32::from_rgb(0x3d, 0xa5, 0xd9),
        on_accent: Color32::from_rgb(0x00, 0x00, 0x00),
        green: Color32::from_rgb(0x00, 0xc8, 0x53),
        amber: Color32::from_rgb(0xff, 0x98, 0x00),
        red: Color32::from_rgb(0xff, 0x3b, 0x30),
        grey: Color32::from_rgb(0x90, 0xa4, 0xae),
        succeeded: Color32::from_rgb(0x4f, 0xc3, 0xf7),
        bg: Color32::from_rgb(0x00, 0x00, 0x00),
        bg_raised: Color32::from_rgb(0x12, 0x12, 0x12),
        stroke: Color32::from_rgb(0xff, 0xff, 0xff),
        canvas_bg: Color32::from_rgb(0x00, 0x00, 0x00),
        text_dim: Color32::from_rgb(0xcf, 0xd8, 0xdc),
        extreme_bg: Color32::from_rgb(0x00, 0x00, 0x00),
        hover_fill: Color32::from_rgb(0x1a, 0x1a, 0x1a),
        hover_text: Color32::from_rgb(0xd3, 0xd9, 0xde),
        node_text: Color32::from_rgb(0xff, 0xff, 0xff),
        tooltip_bg: Color32::from_black_alpha(255),
        tooltip_text: Color32::WHITE,
        service_fill: Color32::from_rgba_unmultiplied_const(0x5b, 0x8d, 0xb8, 60),
        ingress_fill: Color32::from_rgba_unmultiplied_const(0x8d, 0x7a, 0xb8, 60),
        dim_lighten: 0.0,
        dim_divisor: 2,
    }
}

// --- registry + accessors ----------------------------------------------------

static PALETTES: [Palette; 3] = [dark(), light(), high_contrast()];

/// Active palette index; only ever written with a valid `ThemeId` as `u8`.
static CURRENT: AtomicU8 = AtomicU8::new(ThemeId::Dark as u8);

/// The active palette (per-frame hot path; no lock, no allocation).
#[must_use]
pub fn pal() -> &'static Palette {
    &PALETTES[CURRENT.load(Ordering::Relaxed) as usize]
}

/// The active theme id.
#[must_use]
pub fn current() -> ThemeId {
    match CURRENT.load(Ordering::Relaxed) {
        1 => ThemeId::Light,
        2 => ThemeId::HighContrast,
        _ => ThemeId::Dark,
    }
}

/// Set the active palette index without touching the egui context (used at
/// startup so the first `apply` uses the persisted theme).
pub fn set_index(id: ThemeId) {
    CURRENT.store(id as u8, Ordering::Relaxed);
}

/// Apply the active theme's `Visuals` + `Style` to `ctx`.
pub fn apply(ctx: &egui::Context) {
    apply_theme(ctx, current());
}

/// Swap the active theme and re-apply visuals/style (theme-switch UI).
pub fn set_theme(ctx: &egui::Context, id: ThemeId) {
    set_index(id);
    apply_theme(ctx, id);
}

/// Read the persisted theme from `DataDir::standard()/settings.json`.
/// Falls back to [`ThemeId::Dark`] on any failure (missing dir,
/// unparseable, unknown id). Used once by `main` before the window exists.
#[must_use]
pub fn load_persisted_theme() -> ThemeId {
    let settings = kindboard_core::DataDir::standard()
        .ok()
        .and_then(|data_dir| data_dir.load_settings().ok());
    theme_from_settings(settings.as_ref())
}

/// Pure mapping: settings → theme, with the Dark fallback.
fn theme_from_settings(settings: Option<&kindboard_core::Settings>) -> ThemeId {
    settings
        .and_then(|settings| settings.theme.as_deref())
        .and_then(ThemeId::from_id)
        .unwrap_or(ThemeId::DEFAULT)
}

fn apply_theme(ctx: &egui::Context, id: ThemeId) {
    let pal = id.palette();
    let mut visuals = if id.is_dark() {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    visuals.panel_fill = pal.bg;
    visuals.window_fill = pal.bg_raised;
    visuals.window_stroke = egui::Stroke::new(1.0, pal.stroke);
    visuals.selection.bg_fill = pal.accent;
    // R8: selected text is drawn with `Selection::stroke` in egui 0.36 —
    // it must be the on-accent color, not the accent itself, so highlighted
    // text always stays visible.
    visuals.selection.stroke = egui::Stroke::new(1.0, pal.on_accent);
    visuals.hyperlink_color = pal.accent;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, pal.stroke);
    visuals.widgets.inactive.bg_fill = pal.bg_raised;
    visuals.widgets.inactive.weak_bg_fill = pal.bg_raised;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, pal.stroke);
    visuals.widgets.hovered.weak_bg_fill = pal.hover_fill;
    visuals.widgets.hovered.bg_fill = pal.hover_fill;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, pal.accent);
    visuals.widgets.active.weak_bg_fill = pal.accent_hover;
    visuals.widgets.active.bg_fill = pal.accent_hover;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, pal.accent);
    // R8 + ADR-0022: hover text is a gray paired with `hover_fill` (the
    // fill egui actually paints for buttons via `weak_bg_fill`), so labels
    // never disappear on hover, in any theme. The active (pressed) state
    // keeps the darker accent fill, so `on_accent` text stays legible on it
    // (measured ≥ 4.4:1). `open` (combo boxes) keeps its default fill, so
    // its default text color is kept.
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, pal.hover_text);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, pal.on_accent);
    visuals.extreme_bg_color = pal.extreme_bg;

    // Identical Style in every theme — the design language does not change.
    let mut style: Style = (*ctx.global_style()).clone();
    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.window_margin = Margin::same(14);
    style.spacing.scroll = egui::style::ScrollStyle::solid();

    // Cross-platform fix (ADR-0020): `Context::set_visuals`/`set_global_style`
    // mutate only the *currently active* egui theme style, and
    // `theme_preference` defaults to `System`. On Windows and macOS winit
    // reports the OS theme (Light on most machines), so egui renders its
    // untouched default light style — the kindboard palette appeared to not
    // work there, while Fedora (dark OS theme or no theme reported) looked
    // correct. Writing the visuals + style (as one unit, so the style cannot
    // clobber the palette) to BOTH egui themes makes rendering identical on
    // every platform, regardless of the OS theme, including when it changes
    // at runtime.
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.set_style_of(theme, style.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_id_parses_case_insensitive_and_rejects_unknown() {
        assert_eq!(ThemeId::from_id("dark"), Some(ThemeId::Dark));
        assert_eq!(ThemeId::from_id("Dark"), Some(ThemeId::Dark));
        assert_eq!(ThemeId::from_id("DARK"), Some(ThemeId::Dark));
        assert_eq!(ThemeId::from_id("light"), Some(ThemeId::Light));
        assert_eq!(ThemeId::from_id("LIGHT"), Some(ThemeId::Light));
        assert_eq!(
            ThemeId::from_id("high-contrast"),
            Some(ThemeId::HighContrast)
        );
        assert_eq!(
            ThemeId::from_id("High-Contrast"),
            Some(ThemeId::HighContrast)
        );
        assert_eq!(ThemeId::from_id("purple"), None);
        assert_eq!(ThemeId::from_id(""), None);
    }

    #[test]
    fn theme_id_ids_and_labels_are_stable() {
        assert_eq!(ThemeId::Dark.id(), "dark");
        assert_eq!(ThemeId::Light.id(), "light");
        assert_eq!(ThemeId::HighContrast.id(), "high-contrast");
        assert_eq!(ThemeId::Dark.label(), "Dark");
        assert_eq!(ThemeId::Light.label(), "Light");
        assert_eq!(ThemeId::HighContrast.label(), "High Contrast");
        assert!(ThemeId::Dark.is_dark());
        assert!(!ThemeId::Light.is_dark());
        assert!(!ThemeId::HighContrast.is_dark());
    }

    #[test]
    fn dim_matches_per_theme_semantics() {
        let probe = Color32::from_rgb(100, 100, 100);
        assert_eq!(
            ThemeId::Dark.palette().dim(probe),
            Color32::from_rgb(20, 20, 20)
        );
        assert_eq!(
            ThemeId::HighContrast.palette().dim(probe),
            Color32::from_rgb(50, 50, 50)
        );
        assert_eq!(
            ThemeId::Light.palette().dim(probe),
            Color32::from_rgb(224, 224, 224)
        );
    }

    #[test]
    fn all_themes_have_distinct_palettes() {
        let palettes: Vec<&Palette> = ThemeId::ALL.iter().map(|id| id.palette()).collect();
        for (i, a) in palettes.iter().enumerate() {
            for b in &palettes[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    /// Relative luminance of an opaque color (ITU-R BT.709), the same
    /// formula the theme contract uses to measure text contrast.
    fn luminance(color: Color32) -> f64 {
        let channel = |c: u8| {
            let s = f64::from(c) / 255.0;
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
    }

    /// WCAG contrast ratio between two opaque colors.
    fn contrast(a: Color32, b: Color32) -> f64 {
        let (hi, lo) = if luminance(a) >= luminance(b) {
            (luminance(a), luminance(b))
        } else {
            (luminance(b), luminance(a))
        };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn hover_text_is_gray_and_readable_on_hover_fill_in_every_theme() {
        // Regression (ADR-0022): hover text used to be `on_accent` (white on
        // light themes) while egui painted button hover fills from the
        // untouched `hovered.weak_bg_fill` — white letters on a light-gray
        // fill. Hover text must be a gray with ≥ 4.5:1 (WCAG AA) on its own
        // fill in every theme.
        for id in ThemeId::ALL {
            let pal = id.palette();
            let ratio = contrast(pal.hover_text, pal.hover_fill);
            assert!(
                ratio >= 4.5,
                "{:?}: hover_text {:?} on hover_fill {:?} = {:.2}:1 (need ≥ 4.5:1)",
                id,
                pal.hover_text,
                pal.hover_fill,
                ratio
            );
            // It must never be the accent-on color (the old white-letter bug).
            assert_ne!(
                pal.hover_text, pal.on_accent,
                "{id:?}: hover_text must not reuse on_accent"
            );
        }
    }

    #[test]
    fn settings_map_to_theme_with_dark_fallback() {
        // No settings at all → Dark.
        assert_eq!(theme_from_settings(None), ThemeId::Dark);

        // A persisted theme id round-trips.
        let settings = kindboard_core::Settings {
            theme: Some("light".to_string()),
            ..kindboard_core::Settings::default()
        };
        assert_eq!(theme_from_settings(Some(&settings)), ThemeId::Light);
        let settings = kindboard_core::Settings {
            theme: Some("high-contrast".to_string()),
            ..kindboard_core::Settings::default()
        };
        assert_eq!(theme_from_settings(Some(&settings)), ThemeId::HighContrast);

        // Unset theme → Dark.
        let settings = kindboard_core::Settings::default();
        assert_eq!(theme_from_settings(Some(&settings)), ThemeId::Dark);

        // Garbage value → Dark (no panic, forward compatible).
        let settings = kindboard_core::Settings {
            theme: Some("purple".to_string()),
            ..kindboard_core::Settings::default()
        };
        assert_eq!(theme_from_settings(Some(&settings)), ThemeId::Dark);
    }

    // Regression (ADR-0020): on Windows/macOS winit reports the OS theme and
    // `theme_preference` defaults to `System`, so egui can render the *other*
    // (untouched) theme style. `apply` must write the palette + spacing to
    // BOTH egui theme styles so the UI is identical on every platform.

    /// Serialize tests that mutate the process-global `CURRENT` index — the
    /// test harness runs tests on parallel threads, and an unsynchronized
    /// `set_index` in one test would race the `current()` reads in another.
    static THEME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn apply_writes_palette_and_spacing_to_both_egui_themes() {
        let _guard = THEME_TEST_LOCK.lock().unwrap();
        let ctx = egui::Context::default();
        set_index(ThemeId::Dark);
        apply(&ctx);

        let dark_pal = ThemeId::Dark.palette();
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let visuals = &ctx.style_of(theme).visuals;
            assert_eq!(
                visuals.panel_fill, dark_pal.bg,
                "panel_fill mismatch for {theme:?}"
            );
            assert_eq!(visuals.window_fill, dark_pal.bg_raised);
            assert_eq!(visuals.extreme_bg_color, dark_pal.extreme_bg);
            // ADR-0022: hover fill (weak — the one buttons paint) + gray
            // hover text must reach both theme slots.
            assert_eq!(visuals.widgets.hovered.weak_bg_fill, dark_pal.hover_fill);
            assert_eq!(visuals.widgets.hovered.fg_stroke.color, dark_pal.hover_text);
            assert_eq!(
                ctx.style_of(theme).spacing.item_spacing,
                egui::vec2(8.0, 8.0),
                "spacing mismatch for {theme:?}"
            );
        }
    }

    #[test]
    fn set_theme_reapplies_both_egui_themes() {
        let _guard = THEME_TEST_LOCK.lock().unwrap();
        let ctx = egui::Context::default();
        set_index(ThemeId::Dark);
        apply(&ctx);

        // Swap to HighContrast via the real UI path.
        set_theme(&ctx, ThemeId::HighContrast);
        assert_eq!(current(), ThemeId::HighContrast);

        let hc_pal = ThemeId::HighContrast.palette();
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let visuals = &ctx.style_of(theme).visuals;
            assert_eq!(
                visuals.panel_fill, hc_pal.bg,
                "panel_fill mismatch for {theme:?} after set_theme"
            );
        }

        // And back to Light.
        set_theme(&ctx, ThemeId::Light);
        let light_pal = ThemeId::Light.palette();
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            assert_eq!(ctx.style_of(theme).visuals.panel_fill, light_pal.bg);
        }
    }
}
