use anyhow::Result;

use crate::daemon::protocol::{Request, Response};
use crate::daemon::server::{ensure_daemon, send_request};
use crate::output::Format;

pub async fn run(name: String, format: Format) -> Result<()> {
    ensure_daemon()?;

    match send_request(&Request::Status { name }).await? {
        Response::Status { info } => {
            match format {
                Format::Human => {
                    let status = if info.alive {
                        "alive".to_string()
                    } else {
                        format!("exited({})", info.exit_code.unwrap_or(-1))
                    };
                    println!("Name:   {}", info.name);
                    println!("PID:    {}", info.pid);
                    println!("Status: {status}");
                    println!("Size:   {}x{}", info.size.cols, info.size.rows);
                }
                Format::Json => {
                    println!("{}", serde_json::to_string(&info)?);
                }
            }
            Ok(())
        }
        Response::Error { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}
