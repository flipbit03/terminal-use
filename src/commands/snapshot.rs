use anyhow::Result;

use crate::daemon::protocol::{Request, Response};
use crate::daemon::server::{ensure_daemon, send_request};
use crate::output::Format;
use crate::render::text;

pub async fn run(name: String, format: Format) -> Result<()> {
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
                        text::format_snapshot(&content, rows, cols, cursor.row, cursor.col)
                    );
                }
                Format::Json => {
                    println!(
                        "{}",
                        text::format_snapshot_json(&content, rows, cols, cursor.row, cursor.col)
                    );
                }
            }
            Ok(())
        }
        Response::Error { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}
