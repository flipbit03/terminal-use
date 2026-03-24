use anyhow::Result;

use crate::daemon::protocol::{Request, Response, TermSize};
use crate::daemon::server::{ensure_daemon, send_request};
use crate::output::Format;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    command: String,
    args: Vec<String>,
    name: Option<String>,
    size: TermSize,
    scrollback: usize,
    env: Vec<(String, String)>,
    cwd: Option<String>,
    term: String,
    shell: bool,
    format: Format,
) -> Result<()> {
    ensure_daemon()?;

    let resp = send_request(&Request::Run {
        command,
        args,
        name,
        size,
        scrollback,
        env,
        cwd,
        term,
        shell,
    })
    .await?;

    match resp {
        Response::SessionCreated { name, pid } => {
            match format {
                Format::Human => println!("Session {name:?} started (pid {pid})"),
                Format::Json => {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "type": "session_created",
                            "name": name,
                            "pid": pid,
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
