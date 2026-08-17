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

    // Any non-error outcome (Ok, or a connection closed by the shutdown
    // itself) means the daemon acknowledged — wait for it to actually die.
    if let Ok(Response::Error { message }) = server::send_request(&Request::Shutdown).await {
        anyhow::bail!("{message}")
    }

    // The daemon unlinks its socket before acknowledging, but the process and
    // pid file linger a moment longer. Poll until the pid is gone so chains
    // like `tu daemon stop && tu run ...` can never reach the dying daemon.
    for _ in 0..200 {
        if !server::is_daemon_running() {
            println!("Daemon stopped.");
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    anyhow::bail!("daemon acknowledged shutdown but did not exit within 2s")
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
