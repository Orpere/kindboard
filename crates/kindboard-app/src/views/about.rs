//! About window + screenshot capture.
//!
//! Screenshot flow: `ViewportCommand::Screenshot` → eframe repaints and
//! posts `Event::Screenshot` with a `ColorImage` → the app encodes it as
//! PNG (via the `image` crate) into `$KINDBOARD_SCREENSHOT_DIR` (when set)
//! or `~/Pictures/kindboard/` with a timestamped name. Every step is
//! fallible; failures surface as app banners, never panics.

use std::path::PathBuf;

use eframe::egui::{self, ColorImage, RichText};

use crate::theme;

/// Resolve the screenshot directory: `$KINDBOARD_SCREENSHOT_DIR` if set,
/// else `<Pictures>/kindboard`.
pub fn screenshot_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("KINDBOARD_SCREENSHOT_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::picture_dir().map(|pictures| pictures.join("kindboard"))
}

/// Build a timestamped screenshot filename.
pub fn screenshot_path(dir: &PathBuf) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    Some(dir.join(format!("kindboard-{stamp}.png")))
}

/// Encode an egui screenshot into PNG bytes.
pub fn encode_screenshot(image: &ColorImage) -> Result<Vec<u8>, String> {
    let [width, height] = image.size;
    let mut png = image::RgbaImage::new(
        u32::try_from(width).map_err(|_| "image too wide".to_string())?,
        u32::try_from(height).map_err(|_| "image too tall".to_string())?,
    );
    for (index, pixel) in image.pixels.iter().enumerate() {
        let [r, g, b, a] = pixel.to_array();
        let x = (index % width) as u32;
        let y = (index / width) as u32;
        png.put_pixel(x, y, image::Rgba([r, g, b, a]));
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    png.write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|err| format!("png encoding failed: {err}"))?;
    Ok(bytes.into_inner())
}

/// Encode an egui screenshot and save it as a PNG file. Returns the path
/// written.
pub fn save_screenshot(image: &ColorImage) -> Result<PathBuf, String> {
    let dir = screenshot_dir().ok_or_else(|| {
        "no screenshots directory available (set KINDBOARD_SCREENSHOT_DIR or a Pictures dir)"
            .to_string()
    })?;
    let path = screenshot_path(&dir)
        .ok_or_else(|| format!("could not create screenshots directory {}", dir.display()))?;
    let bytes = encode_screenshot(image)?;
    std::fs::write(&path, bytes)
        .map_err(|err| format!("failed to write {}: {err}", path.display()))?;
    Ok(path)
}

/// Encode an egui screenshot and save it to an explicit path, creating
/// parent directories as needed. Returns the path written.
pub fn save_screenshot_to(image: &ColorImage, path: &std::path::Path) -> Result<PathBuf, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    let bytes = encode_screenshot(image)?;
    std::fs::write(path, bytes)
        .map_err(|err| format!("failed to write {}: {err}", path.display()))?;
    Ok(path.to_path_buf())
}

/// Render the About window. Returns `true` when a screenshot was requested
/// (the app sends the viewport command).
pub fn show(ctx: &egui::Context, open: &mut bool) -> bool {
    let mut capture = false;
    let mut close_clicked = false;
    let mut open_local = *open;
    egui::Window::new("About kindboard")
        .open(&mut open_local)
        .resizable(false)
        .default_width(420.0)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                ui.label(RichText::new("kindboard").size(22.0).strong());
                ui.label(
                    RichText::new(format!("version {}", env!("CARGO_PKG_VERSION")))
                        .color(theme::pal().text_dim),
                );
                ui.add_space(10.0);
                ui.label(
                    RichText::new(
                        "Desktop manager for kind clusters: create, inspect and destroy \
                         local Kubernetes development clusters.",
                    )
                    .color(theme::pal().text_dim),
                );
            });
            ui.add_space(12.0);
            ui.separator();
            ui.label(RichText::new("License: MIT").size(12.0));
            ui.label(RichText::new("Copyright 2026 Orlando Rosa Pereira").size(12.0));
            ui.hyperlink_to(
                "github.com/Orpere/kindboard",
                "https://github.com/Orpere/kindboard",
            );
            ui.add_space(6.0);
            ui.separator();
            ui.label(
                RichText::new(
                    "Built with egui/eframe, kind, kube-rs and the Kubernetes ecosystem. \
                     Tool icons are the official project logos (kubectl uses the Kubernetes \
                     logo; kubectx has none). Logos belong to their respective owners.",
                )
                .color(theme::pal().text_dim)
                .size(11.0),
            );
            ui.add_space(10.0);
            ui.vertical_centered(|ui| {
                if ui
                    .add(egui::Button::new("Capture screenshot").min_size(egui::vec2(160.0, 28.0)))
                    .on_hover_text("Save a PNG of this window (see the banner for the path)")
                    .clicked()
                {
                    capture = true;
                }
                if ui.button("Close").clicked() {
                    close_clicked = true;
                }
            });
        });
    if close_clicked {
        open_local = false;
    }
    *open = open_local;
    capture
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    fn tiny_image() -> ColorImage {
        ColorImage {
            size: [2, 2],
            source_size: egui::Vec2::splat(2.0),
            pixels: vec![
                Color32::from_rgb(255, 0, 0),
                Color32::from_rgb(0, 255, 0),
                Color32::from_rgb(0, 0, 255),
                Color32::from_rgb(255, 255, 255),
            ],
        }
    }

    #[test]
    fn screenshot_encodes_valid_png() {
        let bytes = encode_screenshot(&tiny_image()).expect("encode");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory(&bytes).expect("decodable png");
        assert_eq!(decoded.width(), 2);
        assert_eq!(decoded.height(), 2);
    }

    #[test]
    fn screenshot_encoding_preserves_pixels() {
        let bytes = encode_screenshot(&tiny_image()).expect("encode");
        let decoded = image::load_from_memory(&bytes)
            .expect("decodable png")
            .to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0), &image::Rgba([255, 0, 0, 255]));
        assert_eq!(decoded.get_pixel(1, 1), &image::Rgba([255, 255, 255, 255]));
    }

    #[test]
    fn empty_image_rejected() {
        let empty = ColorImage {
            size: [0, 0],
            source_size: egui::Vec2::ZERO,
            pixels: Vec::new(),
        };
        assert!(encode_screenshot(&empty).is_err());
    }
}
