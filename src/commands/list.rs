use anyhow::Result;

use crate::daemon::protocol::{Request, Response};
use crate::daemon::server::{ensure_daemon, send_request};
use crate::output::Format;

pub async fn run(format: Format) -> Result<()> {
    ensure_daemon()?;

    match send_request(&Request::List).await? {
        Response::SessionList { sessions } => {
            match format {
                Format::Human => {
                    if sessions.is_empty() {
                        println!("No active sessions.");
                    } else {
                        println!(
                            "{:<20} {:<10} {:<10} {:<10}",
                            "NAME", "PID", "STATUS", "SIZE"
                        );
                        for s in &sessions {
                            let status = if s.alive {
                                "alive".to_string()
                            } else {
                                format!("exited({})", s.exit_code.unwrap_or(-1))
                            };
                            println!(
                                "{:<20} {:<10} {:<10} {}x{}",
                                s.name, s.pid, status, s.size.cols, s.size.rows
                            );
                        }
                    }
                }
                Format::Json => {
                    println!("{}", serde_json::to_string(&sessions)?);
                }
            }
            Ok(())
        }
        Response::Error { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}
