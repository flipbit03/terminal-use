use anyhow::Result;

use crate::daemon::protocol::{Request, Response};
use crate::daemon::server::{ensure_daemon, send_request};
use crate::keys;

pub async fn run(name: String, key_names: Vec<String>) -> Result<()> {
    ensure_daemon()?;

    let keys = keys::resolve_keys(&key_names)?;

    match send_request(&Request::Press { name, keys }).await? {
        Response::Ok => Ok(()),
        Response::Error { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}
