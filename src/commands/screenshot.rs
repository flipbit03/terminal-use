use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use base64::Engine;

use crate::daemon::protocol::{Request, Response};
use crate::daemon::server::{ensure_daemon, send_request};
use crate::output::Format;
use crate::paths::auto_png_path;
use crate::render::image::Screenshot;
use crate::render::screen::ScreenSnapshot;
use crate::render::text;

pub async fn run_text(name: String, format: Format) -> Result<()> {
    ensure_daemon()?;

    match send_request(&Request::Screenshot { name }).await? {
        Response::Screenshot {
            content,
            rows,
            cols,
            cursor,
            mouse_cursor,
            mouse_held,
        } => {
            match format {
                Format::Human => {
                    println!(
                        "{}",
                        text::format_screenshot(
                            &content,
                            rows,
                            cols,
                            cursor.row,
                            cursor.col,
                            mouse_cursor,
                            mouse_held,
                        )
                    );
                }
                Format::Json => {
                    println!(
                        "{}",
                        text::format_screenshot_json(
                            &content,
                            rows,
                            cols,
                            cursor.row,
                            cursor.col,
                            mouse_cursor,
                            mouse_held,
                        )
                    );
                }
            }
            Ok(())
        }
        Response::Error { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_png(
    name: String,
    output: Option<PathBuf>,
    stdout: bool,
    font: Option<String>,
    font_size: f32,
    show_cursor: bool,
) -> Result<()> {
    ensure_daemon()?;

    let response = send_request(&Request::ScreenshotAnsi { name: name.clone() }).await?;
    let (ansi_bytes, rows, cols, mouse_cursor, mouse_held) = match response {
        Response::ScreenshotAnsi {
            content_b64,
            rows,
            cols,
            mouse_cursor,
            mouse_held,
        } => (
            base64::engine::general_purpose::STANDARD
                .decode(content_b64)
                .context("invalid base64 ANSI snapshot")?,
            rows,
            cols,
            mouse_cursor,
            mouse_held,
        ),
        Response::Error { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    };

    let cursor_for_render = if show_cursor { mouse_cursor } else { None };
    let screenshot = build_screenshot(
        &ansi_bytes,
        rows,
        cols,
        font,
        font_size,
        cursor_for_render,
        mouse_held,
    )?;

    if stdout {
        let png = screenshot.to_png()?;
        std::io::stdout()
            .write_all(&png)
            .context("write PNG screenshot to stdout")?;
        return Ok(());
    }

    let path = output.unwrap_or_else(|| auto_png_path(&name));
    screenshot.save(&path)?;
    println!("{}", path.display());
    Ok(())
}

fn build_screenshot(
    ansi_bytes: &[u8],
    rows: u16,
    cols: u16,
    font: Option<String>,
    font_size: f32,
    mouse_cursor: Option<crate::daemon::protocol::CursorPos>,
    mouse_held: bool,
) -> Result<Screenshot> {
    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(ansi_bytes);

    let screen = ScreenSnapshot::from_vt100(parser.screen());
    let mut screenshot = Screenshot::new(screen).font_size(font_size);
    if let Some(font_path) = font {
        screenshot = screenshot.font_path(&font_path);
    }
    if let Some(pos) = mouse_cursor {
        screenshot = screenshot.mouse_cursor(pos.col, pos.row, mouse_held);
    }

    Ok(screenshot)
}

#[cfg(test)]
mod tests {
    use super::build_screenshot;

    #[test]
    fn build_screenshot_generates_png_bytes() {
        let screenshot =
            build_screenshot(b"\x1b[31mhello\x1b[0m", 4, 20, None, 14.0, None, false).unwrap();
        let png = screenshot.to_png().unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn build_screenshot_with_cursor_overlay_still_emits_png() {
        let cursor = Some(crate::daemon::protocol::CursorPos { col: 2, row: 1 });
        let screenshot =
            build_screenshot(b"\x1b[31mhello\x1b[0m", 4, 20, None, 14.0, cursor, true).unwrap();
        let png = screenshot.to_png().unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}
