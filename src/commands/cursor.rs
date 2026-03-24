use anyhow::Result;

use crate::daemon::protocol::{Request, Response};
use crate::daemon::server::{ensure_daemon, send_request};
use crate::output::Format;

pub async fn run(name: String, format: Format) -> Result<()> {
    ensure_daemon()?;

    match send_request(&Request::Cursor { name }).await? {
        Response::Cursor { pos } => {
            match format {
                Format::Human => println!("{},{}", pos.row, pos.col),
                Format::Json => println!("{}", serde_json::to_string(&pos)?),
            }
            Ok(())
        }
        Response::Error { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}
