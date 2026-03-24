use anyhow::Result;

use crate::daemon::protocol::{Request, Response};
use crate::daemon::server::{ensure_daemon, send_request};
use crate::output::Format;

pub async fn run(name: String, lines: Option<usize>, format: Format) -> Result<()> {
    ensure_daemon()?;

    match send_request(&Request::Scrollback { name, lines }).await? {
        Response::Scrollback { content } => {
            match format {
                Format::Human => println!("{content}"),
                Format::Json => {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "type": "scrollback",
                            "content": content,
                        }))?
                    );
                }
            }
            Ok(())
        }
        Response::Error { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}
