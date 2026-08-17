use anyhow::Result;

use crate::daemon::protocol::{Request, Response};
use crate::daemon::server;

pub async fn start() -> Result<()> {
    if server::is_daemon_running() {
        if server::socket_path().exists() {
            println!("Daemon is already running.");
            return Ok(());
        }
        // Pid alive but no socket: a daemon is mid-teardown (the socket is
        // unlinked before the process exits). Wait briefly for it to die
        // instead of refusing to start — otherwise a concurrent client's
        // ensure_daemon spawn lands here, exits without binding, and the
        // client fails with "daemon did not start in time". If the pid never
        // dies it was reused by an unrelated process (stale pid file);
        // proceed anyway — run_daemon overwrites the pid file.
        for _ in 0..200 {
            if !server::is_daemon_running() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
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
        Ok(Response::Error { message }) => anyhow::bail!("{message}"),
        // Daemon acknowledged — wait for it to actually die below.
        Ok(_) => {}
        Err(_) => {
            // Connect/read failed. If the socket is gone there is no daemon to
            // talk to: either it already tore down, or the pid file is stale
            // (daemon SIGKILLed, pid since reused by an unrelated process) —
            // in which case polling that pid would spin 2s and fail. Clean up
            // and report stopped; the daemon-side guard means a still-dying
            // daemon won't clobber files created after this.
            if !server::socket_path().exists() {
                let _ = std::fs::remove_file(server::pid_path());
                println!("Daemon stopped.");
                return Ok(());
            }
            // Socket still present: connection was likely torn down by the
            // shutdown itself — fall through and wait for the pid to die.
        }
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
