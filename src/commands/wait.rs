use anyhow::Result;

use crate::daemon::protocol::{Request, Response};
use crate::daemon::server::{ensure_daemon, send_request};

pub async fn run(
    name: String,
    stable_ms: Option<u64>,
    text_pattern: Option<String>,
    timeout_ms: u64,
) -> Result<()> {
    ensure_daemon()?;

    match send_request(&Request::Wait {
        name,
        stable_ms,
        text_pattern,
        timeout_ms,
    })
    .await?
    {
        Response::Ok => Ok(()),
        Response::Error { message } => {
            if message == "Wait timed out" {
                // Exit code 1 on timeout (handled by main)
                anyhow::bail!("{message}");
            }
            anyhow::bail!("{message}");
        }
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}
