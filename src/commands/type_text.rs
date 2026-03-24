use anyhow::Result;

use crate::daemon::protocol::{Request, Response};
use crate::daemon::server::{ensure_daemon, send_request};

pub async fn run(name: String, text: String) -> Result<()> {
    ensure_daemon()?;

    match send_request(&Request::Type { name, text }).await? {
        Response::Ok => Ok(()),
        Response::Error { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}
