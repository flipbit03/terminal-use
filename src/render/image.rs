//! Rasterized terminal screenshot renderer.
//!
//! Converts a [`ScreenSnapshot`] into a pixel image by drawing each cell's background
//! rectangle and glyph using an embedded monospace font (JetBrains Mono). The output
//! is a standard RGBA image that can be saved as PNG.
//!
//! A user-specified TTF file can override the embedded font via [`Screenshot::font_path`].

use std::path::Path;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use anyhow::{anyhow, Context, Result};
use image::ImageEncoder;
use image::{ImageBuffer, Rgba, RgbaImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_text_mut};
use imageproc::rect::Rect;

use crate::render::colors::color_to_rgba;
use crate::render::screen::ScreenSnapshot;

/// JetBrains Mono Regular, embedded at compile time (OFL-licensed).
static EMBEDDED_FONT: &[u8] = include_bytes!("fonts/JetBrainsMono-Regular.ttf");

/// Rendering parameters for a terminal screenshot.
///
/// All sizes are in CSS-style pixels. `line_height` is a multiplier on `font_size`
/// (1.2 = 120% line spacing).
#[derive(Debug, Clone)]
pub struct ScreenshotConfig {
    pub font_path: Option<String>,
    pub font_size: f32,
    pub line_height: f32,
    /// If `Some((col, row))` (0-based), paint a magenta overlay on that cell to
    /// show tu's synthetic mouse cursor.
    pub mouse_cursor: Option<(u16, u16)>,
    /// When `mouse_cursor` is set: `true` paints a filled magenta block (a
    /// button is currently held), `false` paints just an outline (idle cursor).
    pub mouse_held: bool,
}

impl Default for ScreenshotConfig {
    fn default() -> Self {
        Self {
            font_path: None,
            font_size: 14.0,
            line_height: 1.2,
            mouse_cursor: None,
            mouse_held: false,
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

    /// Override the font with a TTF file path. If not called, the embedded
    /// JetBrains Mono font is used.
    pub fn font_path(mut self, path: &str) -> Self {
        self.config.font_path = Some(path.to_string());
        self
    }

    /// Override the font size in pixels (default: 14.0).
    pub fn font_size(mut self, size: f32) -> Self {
        self.config.font_size = size;
        self
    }

    /// Paint a magenta overlay on the given cell to show tu's synthetic mouse
    /// cursor. `held = true` draws a filled block (button held), `false` draws
    /// an outline (idle cursor).
    pub fn mouse_cursor(mut self, col: u16, row: u16, held: bool) -> Self {
        self.config.mouse_cursor = Some((col, row));
        self.config.mouse_held = held;
        self
    }

    /// Rasterize the screen to an in-memory RGBA image.
    ///
    /// # Errors
    ///
    /// Returns an error if the font cannot be loaded or parsed.
    pub fn render(&self) -> Result<RgbaImage> {
        render_screen(&self.screen, &self.config)
    }

    /// Render and write to a PNG file.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if ext.as_deref() != Some("png") {
            return Err(anyhow!(
                "screenshot output must be a .png file, got: {}",
                path.display()
            ));
        }
        let image = self.render()?;
        let bytes = encode_png(&image)?;
        std::fs::write(path, bytes)
            .with_context(|| format!("save screenshot to {}", path.display()))?;
        Ok(())
    }

    /// Render and encode as PNG bytes in memory, suitable for piping to stdout.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let image = self.render()?;
        encode_png(&image)
    }
}

fn encode_png(image: &RgbaImage) -> Result<Vec<u8>> {
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

/// Core rasterizer: allocates an image sized to the terminal grid, paints each cell's
/// background, then draws the glyph on top. Character width is derived from the 'M'
/// glyph advance of the loaded font so that the grid is monospaced regardless of which
/// font is used.
fn render_screen(screen: &ScreenSnapshot, config: &ScreenshotConfig) -> Result<RgbaImage> {
    if !config.font_size.is_finite() || config.font_size <= 0.0 {
        return Err(anyhow!("font size must be a finite number greater than 0"));
    }
    if !config.line_height.is_finite() || config.line_height <= 0.0 {
        return Err(anyhow!(
            "line height must be a finite number greater than 0"
        ));
    }

    let font_data: std::borrow::Cow<'_, [u8]> = match &config.font_path {
        Some(path) => {
            let bytes = std::fs::read(path).with_context(|| format!("read font file {path:?}"))?;
            std::borrow::Cow::Owned(bytes)
        }
        None => std::borrow::Cow::Borrowed(EMBEDDED_FONT),
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

    if let Some((cur_col, cur_row)) = config.mouse_cursor {
        if cur_col < screen.cols() && cur_row < screen.rows() {
            paint_mouse_cursor(
                &mut image,
                cur_col,
                cur_row,
                char_width,
                line_height,
                scale,
                &font,
                config.mouse_held,
            );
        }
    }

    Ok(image)
}

/// tu's signature mouse-cursor magenta. Bright enough to spot at a glance,
/// uncommon enough to avoid colliding with typical TUI palettes.
const MOUSE_CURSOR_RGBA: Rgba<u8> = Rgba([255, 0, 200, 255]);
const MOUSE_CURSOR_HELD_FG: Rgba<u8> = Rgba([255, 255, 255, 255]);

/// Paint the synthetic mouse cursor at the given cell as a triangle glyph
/// (`△`). Idle = magenta `△` on the existing background; held =
/// bright-white `△` on a filled magenta cell, so a held button is unmistakable.
#[allow(clippy::too_many_arguments)]
fn paint_mouse_cursor(
    image: &mut RgbaImage,
    cur_col: u16,
    cur_row: u16,
    char_width: f32,
    line_height: f32,
    scale: PxScale,
    font: &FontRef<'_>,
    held: bool,
) {
    let x = (cur_col as f32 * char_width).round() as i32;
    let y = (cur_row as f32 * line_height).round() as i32;
    let w = char_width.ceil() as u32;
    let h = line_height.ceil() as u32;

    let glyph_color = if held {
        // Magenta cell behind a bright-white glyph.
        draw_filled_rect_mut(image, Rect::at(x, y).of_size(w, h), MOUSE_CURSOR_RGBA);
        MOUSE_CURSOR_HELD_FG
    } else {
        // Glyph on top of whatever the inner app drew.
        MOUSE_CURSOR_RGBA
    };

    draw_text_mut(image, glyph_color, x, y, scale, font, "△");
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::Screenshot;
    use crate::render::screen::ScreenSnapshot;

    #[test]
    fn saves_png() {
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
        let gif = base.with_extension("gif");

        screenshot.save(&png).unwrap();
        assert!(screenshot.save(&gif).is_err());

        let _ = std::fs::remove_file(png);
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
