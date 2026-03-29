use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use base64::Engine;

use crate::daemon::protocol::{Request, Response};
use crate::daemon::server::{ensure_daemon, send_request};
use crate::output::Format;
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
        } => {
            match format {
                Format::Human => {
                    println!(
                        "{}",
                        text::format_screenshot(&content, rows, cols, cursor.row, cursor.col)
                    );
                }
                Format::Json => {
                    println!(
                        "{}",
                        text::format_screenshot_json(&content, rows, cols, cursor.row, cursor.col)
                    );
                }
            }
            Ok(())
        }
        Response::Error { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}

fn auto_png_path(session_name: &str) -> PathBuf {
    use std::time::SystemTime;

    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let secs = now.as_secs();
    let tenths = (now.subsec_millis() / 100) as u64;

    // Convert to broken-down time manually (UTC) to avoid adding chrono dep.
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since 1970-01-01 → year/month/day.
    let (year, month, day) = {
        let mut y = 1970i64;
        let mut remaining = days as i64;
        loop {
            let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
                366
            } else {
                365
            };
            if remaining < days_in_year {
                break;
            }
            remaining -= days_in_year;
            y += 1;
        }
        let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        let month_days = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        let mut m = 0usize;
        while m < 12 && remaining >= month_days[m] {
            remaining -= month_days[m];
            m += 1;
        }
        (y, m + 1, remaining + 1)
    };

    let filename = format!(
        "tu-screenshot-{}-{:04}{:02}{:02}T{:02}{:02}{:02}.{}.png",
        session_name, year, month, day, hours, minutes, seconds, tenths
    );
    std::env::temp_dir().join(filename)
}

pub async fn run_png(
    name: String,
    output: Option<PathBuf>,
    stdout: bool,
    font: Option<String>,
    font_size: f32,
) -> Result<()> {
    ensure_daemon()?;

    let response = send_request(&Request::ScreenshotAnsi { name: name.clone() }).await?;
    let (ansi_bytes, rows, cols) = match response {
        Response::ScreenshotAnsi {
            content_b64,
            rows,
            cols,
        } => (
            base64::engine::general_purpose::STANDARD
                .decode(content_b64)
                .context("invalid base64 ANSI snapshot")?,
            rows,
            cols,
        ),
        Response::Error { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    };

    let screenshot = build_screenshot(&ansi_bytes, rows, cols, font, font_size)?;

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
) -> Result<Screenshot> {
    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(ansi_bytes);

    let screen = ScreenSnapshot::from_vt100(parser.screen());
    let mut screenshot = Screenshot::new(screen).font_size(font_size);
    if let Some(font_path) = font {
        screenshot = screenshot.font_path(&font_path);
    }

    Ok(screenshot)
}

#[cfg(test)]
mod tests {
    use super::build_screenshot;

    #[test]
    fn build_screenshot_generates_png_bytes() {
        let screenshot = build_screenshot(b"\x1b[31mhello\x1b[0m", 4, 20, None, 14.0).unwrap();
        let png = screenshot.to_png().unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}
