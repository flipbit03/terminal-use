use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio::time::Duration;

use crate::daemon::manager::SessionManager;
use crate::daemon::protocol::{Request, Response};

/// Returns the socket path for the daemon.
pub fn socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(dir).join("tu.sock")
    } else {
        let uid = nix::unistd::getuid();
        PathBuf::from(format!("/tmp/tu-{uid}.sock"))
    }
}

/// Returns the PID file path.
pub fn pid_path() -> PathBuf {
    let sock = socket_path();
    sock.with_extension("pid")
}

/// Check if the daemon is already running.
pub fn is_daemon_running() -> bool {
    let pid_file = pid_path();
    if let Ok(contents) = std::fs::read_to_string(&pid_file) {
        if let Ok(pid) = contents.trim().parse::<i32>() {
            return nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok();
        }
    }
    false
}

/// Start the daemon as a background process.
pub fn start_daemon_background() -> Result<()> {
    let exe = std::env::current_exe().context("cannot find own executable")?;
    std::process::Command::new(exe)
        .arg("daemon")
        .arg("start")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to spawn daemon")?;

    let sock = socket_path();
    for _ in 0..50 {
        if sock.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    anyhow::bail!("daemon did not start in time")
}

/// Ensure the daemon is running, starting it if needed.
pub fn ensure_daemon() -> Result<()> {
    if is_daemon_running() && socket_path().exists() {
        return Ok(());
    }
    let sock = socket_path();
    if sock.exists() {
        let _ = std::fs::remove_file(&sock);
    }
    start_daemon_background()
}

/// Send a request to the daemon and receive a response.
pub async fn send_request(req: &Request) -> Result<Response> {
    let sock = socket_path();
    let stream = tokio::net::UnixStream::connect(&sock)
        .await
        .context("cannot connect to daemon — is it running?")?;

    let (reader, mut writer) = stream.into_split();

    let mut json = serde_json::to_string(req)?;
    json.push('\n');
    writer
        .write_all(json.as_bytes())
        .await
        .context("write to daemon")?;
    writer.flush().await?;

    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    buf_reader
        .read_line(&mut line)
        .await
        .context("read from daemon")?;

    let response: Response = serde_json::from_str(&line).context("parse daemon response")?;
    Ok(response)
}

/// Run the daemon server (foreground — called from `daemon start`).
pub async fn run_daemon() -> Result<()> {
    let sock = socket_path();
    let pid_file = pid_path();

    if sock.exists() {
        let _ = std::fs::remove_file(&sock);
    }

    std::fs::write(&pid_file, std::process::id().to_string())?;

    let listener = UnixListener::bind(&sock).context("bind socket")?;
    let manager = Arc::new(Mutex::new(SessionManager::new()));

    let idle_timeout = std::env::var("TU_IDLE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(28800); // 8 hours
    let idle_timeout_dur = Duration::from_secs(idle_timeout);

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let mgr = manager.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, &mgr).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("accept error: {e}");
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(10)) => {
                let mgr = manager.lock().await;
                if mgr.session_count() == 0 && mgr.idle_duration() >= idle_timeout_dur {
                    drop(mgr);
                    break;
                }
            }
        }
    }

    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_file(&pid_file);
    Ok(())
}

async fn handle_connection(stream: tokio::net::UnixStream, manager: &Mutex<SessionManager>) {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    match buf_reader.read_line(&mut line).await {
        Ok(0) => return,
        Ok(_) => {}
        Err(e) => {
            eprintln!("read error: {e}");
            return;
        }
    }

    let request: Request = match serde_json::from_str(&line) {
        Ok(req) => req,
        Err(e) => {
            let resp = Response::Error {
                message: format!("Invalid request: {e}"),
            };
            let mut json = serde_json::to_string(&resp).unwrap();
            json.push('\n');
            let _ = writer.write_all(json.as_bytes()).await;
            return;
        }
    };

    let is_shutdown = matches!(request, Request::Shutdown);

    let response = {
        let mut mgr = manager.lock().await;
        mgr.handle(request).await
    };

    let mut json = serde_json::to_string(&response).unwrap();
    json.push('\n');
    let _ = writer.write_all(json.as_bytes()).await;
    let _ = writer.flush().await;

    if is_shutdown {
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::process::exit(0);
    }
}
