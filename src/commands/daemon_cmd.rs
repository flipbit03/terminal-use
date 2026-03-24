use anyhow::Result;

use crate::daemon::protocol::{Request, Response};
use crate::daemon::server;

pub async fn start() -> Result<()> {
    if server::is_daemon_running() {
        println!("Daemon is already running.");
        return Ok(());
    }
    // Run in foreground (this is called by the background spawn)
    server::run_daemon().await
}

pub async fn stop() -> Result<()> {
    if !server::is_daemon_running() {
        println!("Daemon is not running.");
        return Ok(());
    }

    match server::send_request(&Request::Shutdown).await {
        Ok(Response::Ok) => {
            println!("Daemon stopped.");
            Ok(())
        }
        Ok(Response::Error { message }) => anyhow::bail!("{message}"),
        Ok(_) => Ok(()),
        Err(_) => {
            // Connection may have been closed by shutdown
            println!("Daemon stopped.");
            Ok(())
        }
    }
}

pub async fn status() -> Result<()> {
    if server::is_daemon_running() {
        let pid_file = server::pid_path();
        let pid = std::fs::read_to_string(&pid_file).unwrap_or_default();
        println!("Daemon is running (pid {}).", pid.trim());

        // Get session count
        if let Ok(Response::SessionList { sessions }) = server::send_request(&Request::List).await {
            println!("Active sessions: {}", sessions.len());
        }
    } else {
        println!("Daemon is not running.");
    }
    Ok(())
}
