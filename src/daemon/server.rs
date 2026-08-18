use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use regex::Regex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};

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
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);

    let idle_timeout = std::env::var("TU_IDLE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(28800); // 8 hours
    let idle_timeout_dur = Duration::from_secs(idle_timeout);

    let mut shutdown_requested = false;
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let mgr = manager.clone();
                        let tx = shutdown_tx.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, &mgr, tx).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("accept error: {e}");
                    }
                }
            }
            _ = shutdown_rx.recv() => {
                shutdown_requested = true;
                break;
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

    // On Shutdown the connection handler already unlinked the socket (before
    // replying), and a replacement daemon may have bound a new one since —
    // only remove it ourselves in the idle-timeout path. Same care for the
    // pid file: remove it only if it still holds our pid.
    if !shutdown_requested {
        let _ = std::fs::remove_file(&sock);
    }
    let my_pid = std::process::id().to_string();
    if std::fs::read_to_string(&pid_file).is_ok_and(|c| c.trim() == my_pid) {
        let _ = std::fs::remove_file(&pid_file);
    }
    Ok(())
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    manager: &Mutex<SessionManager>,
    shutdown_tx: tokio::sync::mpsc::Sender<()>,
) {
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

    // Wait and Mouse run outside the manager lock so they don't block other
    // requests. Wait polls for up to a few seconds; Mouse paces interpolated
    // motion events with sleeps so the synthetic cursor visibly glides in
    // `tu monitor` and the inner app sees real motion rather than a teleport.
    let response = match request {
        Request::Wait {
            name,
            stable_ms,
            text_pattern,
            timeout_ms,
        } => handle_wait(manager, &name, stable_ms, text_pattern, timeout_ms).await,
        Request::Mouse {
            name,
            action,
            force,
        } => crate::daemon::manager::handle_mouse_glided(manager, name, action, force).await,
        other => {
            let mut mgr = manager.lock().await;
            mgr.handle(other).await
        }
    };

    // Unlink the socket before acknowledging a shutdown so no new client can
    // connect to (and create sessions on) a dying daemon. The reply still
    // flushes fine over the already-accepted connection.
    if is_shutdown {
        let _ = std::fs::remove_file(socket_path());
    }

    let mut json = serde_json::to_string(&response).unwrap();
    json.push('\n');
    let _ = writer.write_all(json.as_bytes()).await;
    let _ = writer.flush().await;

    if is_shutdown {
        // Exit via the normal end of run_daemon (accept loop breaks, pid file
        // cleanup runs) instead of std::process::exit(0) here.
        let _ = shutdown_tx.send(()).await;
    }
}

/// Handle the Wait request without holding the SessionManager lock during the
/// polling loop. We briefly lock the manager to look up the session's parser
/// and size, then drop the lock and poll using the `Arc<Mutex<Parser>>` directly.
async fn handle_wait(
    manager: &Mutex<SessionManager>,
    name: &str,
    stable_ms: Option<u64>,
    text_pattern: Option<String>,
    timeout_ms: u64,
) -> Response {
    // Briefly lock the manager to validate the session and get a reference to its parser.
    let (parser, size) = {
        let mut mgr = manager.lock().await;
        mgr.touch();
        match mgr.get_session_parser(name) {
            Some(refs) => refs,
            None => {
                return Response::Error {
                    message: format!("Session {name:?} not found"),
                }
            }
        }
    };

    let compiled_regex = if let Some(ref pat) = text_pattern {
        match Regex::new(pat) {
            Ok(re) => Some(re),
            Err(e) => {
                return Response::Error {
                    message: format!("Invalid regex {pat:?}: {e}"),
                }
            }
        }
    } else {
        None
    };

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let stable_duration = stable_ms.map(Duration::from_millis);
    let mut last_content: Option<String> = None;
    let mut stable_since: Option<Instant> = None;

    loop {
        if Instant::now() >= deadline {
            return Response::Error {
                message: "Wait timed out".into(),
            };
        }

        // Read the screenshot using the session's parser directly (no manager lock needed).
        let content = screenshot_text_from_parser(&parser, &size).await;

        // Check text pattern
        if let Some(ref re) = compiled_regex {
            if re.is_match(&content) {
                return Response::Ok;
            }
        }

        // Check stability
        if let Some(stable_dur) = stable_duration {
            match &last_content {
                Some(prev) if prev == &content => {
                    if let Some(since) = stable_since {
                        if since.elapsed() >= stable_dur {
                            return Response::Ok;
                        }
                    }
                }
                _ => {
                    last_content = Some(content);
                    stable_since = Some(Instant::now());
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Read the screen contents as plain text from a parser, mirroring
/// `Session::screenshot_text()` — uses the shared `Screen::text_rows`
/// builder so wait, screenshots, and mouse target resolution all see the
/// exact same view of the screen.
async fn screenshot_text_from_parser(
    parser: &Mutex<crate::emu::Parser>,
    _size: &crate::daemon::protocol::TermSize,
) -> String {
    let parser = parser.lock().await;
    let mut lines: Vec<String> = parser
        .screen()
        .text_rows()
        .into_iter()
        .map(|l| l.trim_end().to_string())
        .collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}
