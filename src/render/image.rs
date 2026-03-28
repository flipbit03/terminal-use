//! Rasterized terminal screenshot renderer.
//!
//! Converts a [`ScreenSnapshot`] into a pixel image by drawing each cell's background
//! rectangle and glyph using a system monospace font. The output is a standard RGBA image
//! that can be saved as PNG or JPEG.
//!
//! Font discovery happens via `font-kit`'s [`SystemSource`], which queries the OS font
//! registry for a monospace family (or a user-specified family). The font is loaded fresh
//! on every render call — there is no cross-invocation cache, which is acceptable for a
//! short-lived CLI process but would need revisiting for batch use.

use std::path::Path;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use anyhow::{anyhow, Context, Result};
use font_kit::{family_name::FamilyName, handle::Handle, source::SystemSource};
use image::ImageEncoder;
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba, RgbaImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_text_mut};
use imageproc::rect::Rect;

use crate::render::colors::color_to_rgba;
use crate::render::screen::ScreenSnapshot;

/// Rendering parameters for a terminal screenshot.
///
/// All sizes are in CSS-style pixels. `line_height` is a multiplier on `font_size`
/// (1.2 = 120% line spacing).
#[derive(Debug, Clone)]
pub struct ScreenshotConfig {
    pub font_name: Option<String>,
    pub font_size: f32,
    pub line_height: f32,
}

impl Default for ScreenshotConfig {
    fn default() -> Self {
        Self {
            font_name: None,
            font_size: 14.0,
            line_height: 1.2,
        }
    }
}

/// Builder for rendering a terminal screen to a raster image.
///
/// Construct with [`Screenshot::new`], optionally override font settings with the
/// builder methods, then call [`Screenshot::save`] or [`Screenshot::to_png`].
pub struct Screenshot {
    screen: ScreenSnapshot,
    config: ScreenshotConfig,
}

impl Screenshot {
    /// Create a screenshot renderer for the given screen snapshot with default config.
    pub fn new(screen: ScreenSnapshot) -> Self {
        Self {
            screen,
            config: ScreenshotConfig::default(),
        }
    }

    /// Override the font family. If not called, the system's default monospace font is used.
    pub fn font_name(mut self, name: &str) -> Self {
        self.config.font_name = Some(name.to_string());
        self
    }

    /// Override the font size in pixels (default: 14.0).
    pub fn font_size(mut self, size: f32) -> Self {
        self.config.font_size = size;
        self
    }

    /// Rasterize the screen to an in-memory RGBA image.
    ///
    /// # Errors
    ///
    /// Returns an error if the system font cannot be loaded or parsed.
    pub fn render(&self) -> Result<RgbaImage> {
        render_screen(&self.screen, &self.config)
    }

    /// Render and write to a file. The format is inferred from the extension
    /// (`.png`, `.jpg`, `.jpeg`).
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let image = DynamicImage::ImageRgba8(self.render()?);
        let format = output_format(path)?;
        image
            .save_with_format(path, format)
            .with_context(|| format!("save screenshot to {}", path.display()))?;
        Ok(())
    }

    /// Render and encode as PNG bytes in memory, suitable for piping to stdout.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let image = self.render()?;
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ColorType::Rgba8.into(),
            )
            .context("encode PNG screenshot")?;
        Ok(bytes)
    }
}

/// Map a file extension to an image format. Only PNG and JPEG are supported because
/// these are the formats commonly accepted by browsers and chat tools for inline display.
fn output_format(path: &Path) -> Result<ImageFormat> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .ok_or_else(|| anyhow!("screenshot output path must include .png, .jpg, or .jpeg"))?;

    match ext.as_str() {
        "png" => Ok(ImageFormat::Png),
        "jpg" | "jpeg" => Ok(ImageFormat::Jpeg),
        _ => Err(anyhow!(
            "unsupported screenshot output extension .{ext}; use .png, .jpg, or .jpeg"
        )),
    }
}

/// Core rasterizer: allocates an image sized to the terminal grid, paints each cell's
/// background, then draws the glyph on top. Character width is derived from the 'M'
/// glyph advance of the loaded font so that the grid is monospaced regardless of which
/// font the OS provides.
fn render_screen(screen: &ScreenSnapshot, config: &ScreenshotConfig) -> Result<RgbaImage> {
    if !config.font_size.is_finite() || config.font_size <= 0.0 {
        return Err(anyhow!("font size must be a finite number greater than 0"));
    }
    if !config.line_height.is_finite() || config.line_height <= 0.0 {
        return Err(anyhow!(
            "line height must be a finite number greater than 0"
        ));
    }

    let source = SystemSource::new();
    let handle = match &config.font_name {
        Some(name) => source
            .select_best_match(&[FamilyName::Title(name.clone())], &Default::default())
            .map_err(|err| anyhow!("load font {name:?}: {err}"))?,
        None => source
            .select_best_match(&[FamilyName::Monospace], &Default::default())
            .map_err(|err| anyhow!("load system monospace font: {err}"))?,
    };

    let font_data = match handle {
        Handle::Path { path, .. } => {
            std::fs::read(&path).with_context(|| format!("read font {}", path.display()))?
        }
        Handle::Memory { bytes, .. } => bytes.to_vec(),
    };

    let font =
        FontRef::try_from_slice(&font_data).map_err(|err| anyhow!("parse font data: {err}"))?;

    let scale = PxScale::from(config.font_size);
    let scaled_font = font.as_scaled(scale);
    let line_height = config.font_size * config.line_height;
    let char_width = scaled_font.h_advance(font.glyph_id('M'));

    let width = (screen.cols() as f32 * char_width).ceil() as u32;
    let height = (screen.rows() as f32 * line_height).ceil() as u32;
    let mut image: RgbaImage = ImageBuffer::new(width, height);
    let full_rect = Rect::at(0, 0).of_size(width, height);
    draw_filled_rect_mut(&mut image, full_rect, Rgba([0, 0, 0, 255]));

    for (row_idx, row) in screen.cells().iter().enumerate() {
        let y = row_idx as f32 * line_height;

        for (col_idx, cell) in row.iter().enumerate() {
            let x = col_idx as f32 * char_width;
            let bg = if cell.attrs.inverse {
                color_to_rgba(cell.fg, true)
            } else {
                color_to_rgba(cell.bg, false)
            };

            let rect = Rect::at(x.round() as i32, y.round() as i32)
                .of_size(char_width.ceil() as u32, line_height.ceil() as u32);
            draw_filled_rect_mut(&mut image, rect, bg);
        }
    }

    for (row_idx, row) in screen.cells().iter().enumerate() {
        let y = row_idx as f32 * line_height;

        for (col_idx, cell) in row.iter().enumerate() {
            let x = col_idx as f32 * char_width;
            let fg = if cell.attrs.inverse {
                color_to_rgba(cell.bg, false)
            } else {
                color_to_rgba(cell.fg, true)
            };

            if cell.is_wide_continuation || cell.contents.is_empty() || cell.contents == " " {
                continue;
            }

            draw_text_mut(
                &mut image,
                fg,
                x.round() as i32,
                y.round() as i32,
                scale,
                &font,
                &cell.contents,
            );
        }
    }

    Ok(image)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::Screenshot;
    use crate::render::screen::ScreenSnapshot;

    #[test]
    fn saves_png_and_jpeg() {
        let mut parser = vt100::Parser::new(4, 20, 0);
        parser.process(b"\x1b[32mhello\x1b[0m");
        let screenshot = Screenshot::new(ScreenSnapshot::from_vt100(parser.screen()));

        let base = std::env::temp_dir().join(format!(
            "tu-screenshot-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let png = base.with_extension("png");
        let jpg = base.with_extension("jpg");
        let bad = base.with_extension("gif");

        screenshot.save(&png).unwrap();
        screenshot.save(&jpg).unwrap();
        assert!(screenshot.save(&bad).is_err());

        let _ = std::fs::remove_file(png);
        let _ = std::fs::remove_file(jpg);
    }

    #[test]
    fn rejects_invalid_font_size() {
        let mut parser = vt100::Parser::new(2, 4, 0);
        parser.process(b"hi");
        let screenshot =
            Screenshot::new(ScreenSnapshot::from_vt100(parser.screen())).font_size(0.0);
        assert!(screenshot.render().is_err());
    }
}
