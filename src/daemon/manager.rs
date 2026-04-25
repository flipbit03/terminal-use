use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};

use crate::daemon::protocol::{
    MouseAction, MouseButton, MouseEncoding, MouseEventKind, MouseLastEvent, MouseMode, MouseMods,
    MouseTarget, Request, Response, SessionInfo, TermSize,
};
use crate::daemon::session::Session;
use crate::mouse::{self, WireEvent};

/// Manages all terminal sessions.
pub struct SessionManager {
    sessions: HashMap<String, Session>,
    last_activity: Instant,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            last_activity: Instant::now(),
        }
    }

    /// Process a request and return a response.
    pub async fn handle(&mut self, req: Request) -> Response {
        self.last_activity = Instant::now();

        match req {
            Request::Run {
                command,
                args,
                name,
                size,
                scrollback,
                env,
                cwd,
                term,
                shell,
            } => self.handle_run(command, args, name, size, scrollback, env, cwd, term, shell),

            Request::Kill { name } => self.handle_kill(&name),

            Request::List => self.handle_list(),

            Request::Status { name } => self.handle_status(&name),

            Request::Screenshot { name } => self.handle_screenshot(&name).await,

            Request::ScreenshotAnsi { name } => self.handle_screenshot_ansi(&name).await,

            Request::ScreenshotCells { name } => self.handle_screenshot_cells(&name).await,

            Request::Cursor { name } => self.handle_cursor(&name).await,

            Request::Scrollback { name, lines } => self.handle_scrollback(&name, lines).await,

            Request::Type { name, text } => self.handle_type(&name, &text),

            Request::Press { name, keys } => self.handle_press(&name, &keys),

            Request::Paste { name, text } => self.handle_paste(&name, &text),

            Request::Resize { name, size } => self.handle_resize(&name, size).await,

            Request::MouseState { name } => self.handle_mouse_state(&name).await,

            // Wait and Mouse are handled in server.rs without the manager lock —
            // Wait polls for seconds, Mouse paces interpolated motion events
            // with sleeps so the synthetic cursor visibly glides on monitor.
            Request::Wait { .. } => unreachable!("Wait should be handled in server.rs"),
            Request::Mouse { .. } => unreachable!("Mouse should be handled in server.rs"),

            Request::Shutdown => {
                // Kill all sessions
                let names: Vec<String> = self.sessions.keys().cloned().collect();
                for name in names {
                    if let Some(mut session) = self.sessions.remove(&name) {
                        session.kill();
                    }
                }
                Response::Ok
            }
        }
    }

    /// How long since the last request.
    pub fn idle_duration(&self) -> Duration {
        self.last_activity.elapsed()
    }

    /// Record that activity just happened (used by wait handler in server.rs).
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Number of active sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Get a clone of the session's vt100 parser and its terminal size.
    /// Used by the wait handler in server.rs to read screenshots without holding
    /// the manager lock.
    pub fn get_session_parser(
        &self,
        name: &str,
    ) -> Option<(Arc<Mutex<crate::emu::Parser>>, TermSize)> {
        self.sessions
            .get(name)
            .map(|s| (s.parser.clone(), s.size.clone()))
    }

    fn allocate_name(&self, requested: Option<String>) -> String {
        match requested {
            Some(name) => name,
            None => {
                if !self.sessions.contains_key("default") {
                    return "default".into();
                }
                let mut i = 1;
                loop {
                    let name = format!("session-{i}");
                    if !self.sessions.contains_key(&name) {
                        return name;
                    }
                    i += 1;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_run(
        &mut self,
        command: String,
        args: Vec<String>,
        name: Option<String>,
        size: TermSize,
        scrollback: usize,
        env: Vec<(String, String)>,
        cwd: Option<String>,
        term: String,
        shell: bool,
    ) -> Response {
        let session_name = self.allocate_name(name);

        if self.sessions.contains_key(&session_name) {
            return Response::Error {
                message: format!("Session {session_name:?} already exists"),
            };
        }

        match Session::new(
            session_name.clone(),
            &command,
            &args,
            size,
            scrollback,
            &env,
            cwd.as_deref(),
            &term,
            shell,
        ) {
            Ok(session) => {
                if let Err(e) = session.start_reader() {
                    return Response::Error {
                        message: format!("Failed to start PTY reader: {e}"),
                    };
                }
                let pid = session.pid.as_raw() as u32;
                self.sessions.insert(session_name.clone(), session);
                Response::SessionCreated {
                    name: session_name,
                    pid,
                }
            }
            Err(e) => Response::Error {
                message: format!("Failed to spawn process: {e}"),
            },
        }
    }

    fn handle_kill(&mut self, name: &str) -> Response {
        match self.sessions.remove(name) {
            Some(mut session) => {
                session.kill();
                Response::Ok
            }
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }

    fn handle_list(&mut self) -> Response {
        let mut sessions: Vec<SessionInfo> = Vec::new();
        for session in self.sessions.values_mut() {
            sessions.push(session.info());
        }
        sessions.sort_by(|a, b| a.name.cmp(&b.name));
        Response::SessionList { sessions }
    }

    fn handle_status(&mut self, name: &str) -> Response {
        match self.sessions.get_mut(name) {
            Some(session) => Response::Status {
                info: session.info(),
            },
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }

    async fn handle_screenshot(&mut self, name: &str) -> Response {
        match self.sessions.get_mut(name) {
            Some(session) => {
                session.poll_status();
                let content = session.screenshot_text().await;
                let cursor = session.cursor_pos().await;
                Response::Screenshot {
                    content,
                    rows: session.size.rows,
                    cols: session.size.cols,
                    cursor,
                    mouse_cursor: session.mouse.cursor,
                    mouse_held: !session.mouse.buttons_held.is_empty(),
                }
            }
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }

    async fn handle_screenshot_ansi(&mut self, name: &str) -> Response {
        match self.sessions.get_mut(name) {
            Some(session) => {
                session.poll_status();
                let ansi_bytes = session.screenshot_ansi().await;
                use base64::Engine;
                let content_b64 = base64::engine::general_purpose::STANDARD.encode(&ansi_bytes);
                Response::ScreenshotAnsi {
                    content_b64,
                    rows: session.size.rows,
                    cols: session.size.cols,
                    mouse_cursor: session.mouse.cursor,
                    mouse_held: !session.mouse.buttons_held.is_empty(),
                }
            }
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }

    async fn handle_screenshot_cells(&mut self, name: &str) -> Response {
        match self.sessions.get_mut(name) {
            Some(session) => {
                session.poll_status();
                let rows_ansi = session.screenshot_cells().await;
                Response::ScreenshotCells {
                    rows_ansi,
                    rows: session.size.rows,
                    cols: session.size.cols,
                    mouse_cursor: session.mouse.cursor,
                    mouse_held: !session.mouse.buttons_held.is_empty(),
                }
            }
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }

    async fn handle_cursor(&self, name: &str) -> Response {
        match self.sessions.get(name) {
            Some(session) => Response::Cursor {
                pos: session.cursor_pos().await,
            },
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }

    async fn handle_scrollback(&self, name: &str, lines: Option<usize>) -> Response {
        match self.sessions.get(name) {
            Some(session) => Response::Scrollback {
                content: session.scrollback(lines).await,
            },
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }

    fn handle_type(&self, name: &str, text: &str) -> Response {
        match self.sessions.get(name) {
            Some(session) => match session.type_text(text) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error {
                    message: format!("Type failed: {e}"),
                },
            },
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }

    fn handle_press(&self, name: &str, keys: &[u8]) -> Response {
        match self.sessions.get(name) {
            Some(session) => match session.write_bytes(keys) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error {
                    message: format!("Press failed: {e}"),
                },
            },
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }

    fn handle_paste(&self, name: &str, text: &str) -> Response {
        match self.sessions.get(name) {
            Some(session) => match session.paste_text(text) {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error {
                    message: format!("Paste failed: {e}"),
                },
            },
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }

    async fn handle_resize(&mut self, name: &str, size: TermSize) -> Response {
        match self.sessions.get_mut(name) {
            Some(session) => match session.resize(size).await {
                Ok(()) => Response::Ok,
                Err(e) => Response::Error {
                    message: format!("Resize failed: {e}"),
                },
            },
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }

    async fn handle_mouse_state(&self, name: &str) -> Response {
        match self.sessions.get(name) {
            Some(session) => {
                let parser = session.parser.lock().await;
                let screen = parser.screen();
                Response::MouseState {
                    mode: vt_mode_to_proto(screen.mouse_protocol_mode()),
                    encoding: vt_encoding_to_proto(screen.mouse_protocol_encoding()),
                    size: session.size.clone(),
                    cursor: session.mouse.cursor,
                    buttons_held: session.mouse.buttons_held.clone(),
                    last_event: session.mouse.last_event.clone(),
                }
            }
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }
}

/// Process a Mouse request without holding the manager lock for the whole
/// command. Click / Down / Up / Move *interpolate* a path of motion events
/// from the synthetic cursor's current position to the target — paced with
/// short sleeps so monitor's `△` glides instead of teleporting and the inner
/// app sees a real-mouse-style stream of motion events.
///
/// Drag continues to interpolate its own from→to segment internally, after
/// gliding cur→from. Scroll is position-independent and skips the glide.
///
/// The manager lock is taken briefly at start (snapshot session refs) and
/// briefly per emit (tracker update). Between emits the lock is released so
/// monitor's `ScreenshotCells` polls can interleave and pick up each
/// intermediate cursor position.
pub async fn handle_mouse_glided(
    manager: &Mutex<SessionManager>,
    name: String,
    action: MouseAction,
    force: bool,
) -> Response {
    use std::os::fd::OwnedFd;

    // Step 1: snapshot session state under one brief lock. We dup the master
    // fd here so we can write to the PTY between manager-lock acquisitions
    // without keeping a session reference alive.
    let snapshot = {
        let mut mgr = manager.lock().await;
        // Reset the idle timer so a long stream of mouse activity (drag,
        // scroll, click sequences) doesn't let the daemon time out.
        mgr.touch();
        let Some(session) = mgr.sessions.get(&name) else {
            return Response::Error {
                message: format!("Session {name:?} not found"),
            };
        };
        let parser = session.parser.clone();
        let cols = session.size.cols;
        let rows = session.size.rows;
        let cur_pos = session.mouse.cursor;
        let buttons_held: Vec<MouseButton> = session.mouse.buttons_held.clone();
        let master_fd: OwnedFd = match nix::unistd::dup(&session.master_fd) {
            Ok(fd) => fd,
            Err(e) => {
                return Response::Error {
                    message: format!("dup master_fd: {e}"),
                }
            }
        };
        let (mode, encoding, screen_rows) = {
            let p = parser.lock().await;
            let screen = p.screen();
            (
                vt_mode_to_proto(screen.mouse_protocol_mode()),
                vt_encoding_to_proto(screen.mouse_protocol_encoding()),
                screen.text_rows(),
            )
        };
        Snapshot {
            master_fd,
            cols,
            rows,
            cur_pos: cur_pos.map(|p| (p.col, p.row)),
            buttons_held,
            mode,
            encoding,
            screen_rows,
        }
    };

    let Snapshot {
        master_fd,
        cols,
        rows,
        cur_pos,
        buttons_held,
        mode,
        encoding,
        screen_rows,
    } = snapshot;

    if !force && mode == MouseMode::None {
        return Response::Error {
            message: format!(
                "session {name:?} has not enabled mouse reporting (DECSET 1000/1002/1006). \
                 Use --force to send raw bytes anyway."
            ),
        };
    }

    // Resolve targets to coordinates against the snapshotted screen text.
    let resolve = |target: &MouseTarget| -> Result<(u16, u16), String> {
        match target {
            MouseTarget::Coords { col, row } => {
                if *col >= cols || *row >= rows {
                    return Err(format!(
                        "coords ({col},{row}) out of bounds (terminal is {cols}x{rows})"
                    ));
                }
                Ok((*col, *row))
            }
            MouseTarget::Text {
                needle,
                match_index,
            } => {
                let hits = mouse::find_text(&screen_rows, needle);
                pick_match(&hits, *match_index, &format!("text {needle:?}"))
            }
            MouseTarget::Regex {
                pattern,
                match_index,
            } => {
                let hits = match mouse::find_regex(&screen_rows, pattern) {
                    Ok(h) => h,
                    Err(e) => return Err(e.to_string()),
                };
                pick_match(&hits, *match_index, &format!("regex {pattern:?}"))
            }
        }
    };

    // Where should the synthetic cursor land BEFORE the action's own events?
    // For most actions: at the action's target. For Drag: at its `from`.
    // Scroll is position-independent so no pre-glide.
    let glide_target: Option<(u16, u16)> = match &action {
        MouseAction::Click { target, .. }
        | MouseAction::Down { target, .. }
        | MouseAction::Up { target, .. }
        | MouseAction::Move { target, .. } => match resolve(target) {
            Ok(p) => Some(p),
            Err(e) => return Response::Error { message: e },
        },
        MouseAction::Drag { from, .. } => match resolve(from) {
            Ok(p) => Some(p),
            Err(e) => return Response::Error { message: e },
        },
        MouseAction::Scroll { .. } => None,
    };

    // Run the glide if we have both a starting cursor and a destination.
    if let (Some(start), Some(end)) = (cur_pos, glide_target) {
        if start != end {
            let path = full_path_inclusive(start.0, start.1, end.0, end.1);
            // Skip the very first cell — that's where the cursor already sits.
            // Include the destination cell so the cursor visibly arrives.
            glide_cells(
                manager,
                &name,
                &master_fd,
                &path[1..],
                &buttons_held,
                mode,
                encoding,
            )
            .await;
        }
    } else if cur_pos.is_none() {
        // First-ever positional command: no glide, but we still want the
        // cursor to land at the action's target. Skipping glide is fine —
        // the action's own events will set the position.
    }

    // Now emit the action's own events (possibly Drag's interpolated segment,
    // or Click's Down + Up at target, etc.). For Move there are no extra
    // events: glide_cells already deposited the final Move at the destination.
    if matches!(action, MouseAction::Move { .. }) {
        return Response::Ok;
    }

    let events = match build_events(&action, &resolve, mode) {
        Ok(evs) => evs,
        Err(e) => return Response::Error { message: e },
    };

    let bytes = match mouse::encode(&events, encoding) {
        Ok(b) => b,
        Err(e) => {
            return Response::Error {
                message: format!("encode mouse events: {e}"),
            }
        }
    };

    if let Err(e) = crate::pty::input::write_to_pty(&master_fd, &bytes) {
        return Response::Error {
            message: format!("Mouse write failed: {e}"),
        };
    }

    // Commit tracker update for the action's events.
    {
        let mut mgr = manager.lock().await;
        if let Some(s) = mgr.sessions.get_mut(&name) {
            update_tracker(&mut s.mouse, &events);
        }
    }

    Response::Ok
}

struct Snapshot {
    master_fd: std::os::fd::OwnedFd,
    cols: u16,
    rows: u16,
    cur_pos: Option<(u16, u16)>,
    buttons_held: Vec<MouseButton>,
    mode: MouseMode,
    encoding: MouseEncoding,
    screen_rows: Vec<String>,
}

/// All cells along a Bresenham-like path from `(c1,r1)` to `(c2,r2)`,
/// inclusive of both endpoints.
fn full_path_inclusive(c1: u16, r1: u16, c2: u16, r2: u16) -> Vec<(u16, u16)> {
    let dx = (c2 as i32 - c1 as i32).abs();
    let dy = (r2 as i32 - r1 as i32).abs();
    let steps = dx.max(dy);
    if steps == 0 {
        return vec![(c1, r1)];
    }
    let mut out = Vec::with_capacity((steps + 1) as usize);
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let col = (c1 as f64 + (c2 as f64 - c1 as f64) * t).round() as u16;
        let row = (r1 as f64 + (r2 as f64 - r1 as f64) * t).round() as u16;
        out.push((col, row));
    }
    out
}

/// Pace through a sequence of cells, emitting motion events on the wire (when
/// the inner app's mouse mode supports it) and updating the per-session
/// cursor tracker so `tu monitor` sees the synthetic cursor glide.
///
/// Bare motion (`Move`) is emitted on the wire only when the inner app has
/// `AnyMotion` (DECSET 1003). With `ButtonMotion` (1002), motion is reported
/// only when a button is held — which is the case if `buttons_held` is
/// non-empty (e.g. a glide before a `mouse up` after a prior `mouse down`).
/// In modes that don't report motion at all the wire emission is skipped, but
/// the tracker still updates so the synthetic cursor still glides visually.
async fn glide_cells(
    manager: &Mutex<SessionManager>,
    name: &str,
    master_fd: &std::os::fd::OwnedFd,
    cells: &[(u16, u16)],
    buttons_held: &[MouseButton],
    mode: MouseMode,
    encoding: MouseEncoding,
) {
    if cells.is_empty() {
        return;
    }
    // Sleep budget: ~6ms per cell, capped at 250ms total. Short enough that
    // even a corner-to-corner glide finishes before the user's next command,
    // long enough to be visibly fluid at 30fps.
    let target_ms: u64 = ((cells.len() as u64) * 6).min(250);
    let per_cell = Duration::from_millis((target_ms / cells.len() as u64).max(1));

    let drag_button = buttons_held.first().copied();
    let mods = MouseMods::default();

    for &(col, row) in cells {
        // Build the per-cell wire event (if the app's mode reports it).
        let wire_event: Option<WireEvent> = match (mode, drag_button) {
            (MouseMode::AnyMotion, Some(button)) => Some(WireEvent::DragMove {
                col,
                row,
                button,
                mods,
            }),
            (MouseMode::AnyMotion, None) => Some(WireEvent::Move { col, row, mods }),
            (MouseMode::ButtonMotion, Some(button)) => Some(WireEvent::DragMove {
                col,
                row,
                button,
                mods,
            }),
            // ButtonMotion without a held button, or PressRelease/Press/None:
            // the app doesn't expect motion here. Skip wire emission but still
            // update the synthetic-cursor tracker so monitor glides.
            _ => None,
        };

        if let Some(ev) = wire_event {
            if let Ok(bytes) = mouse::encode(&[ev], encoding) {
                let _ = crate::pty::input::write_to_pty(master_fd, &bytes);
            }
        }

        {
            let mut mgr = manager.lock().await;
            if let Some(s) = mgr.sessions.get_mut(name) {
                s.mouse.record_position(col, row);
                if let Some(ev) = wire_event {
                    s.mouse.last_event = Some(MouseLastEvent {
                        kind: match ev {
                            WireEvent::DragMove { .. } => MouseEventKind::DragMove,
                            WireEvent::Move { .. } => MouseEventKind::Move,
                            _ => MouseEventKind::Move,
                        },
                        col,
                        row,
                        button: drag_button,
                        scroll_dir: None,
                        mods,
                        ts_unix: now_unix(),
                    });
                }
            }
        }

        tokio::time::sleep(per_cell).await;
    }
}

fn update_tracker(tracker: &mut crate::daemon::session::MouseTracker, events: &[WireEvent]) {
    for ev in events {
        match *ev {
            WireEvent::Down {
                col,
                row,
                button,
                mods,
            } => {
                tracker.record_position(col, row);
                tracker.press(button);
                tracker.last_event = Some(MouseLastEvent {
                    kind: MouseEventKind::Down,
                    col,
                    row,
                    button: Some(button),
                    scroll_dir: None,
                    mods,
                    ts_unix: now_unix(),
                });
            }
            WireEvent::Up {
                col,
                row,
                button,
                mods,
            } => {
                tracker.record_position(col, row);
                tracker.release(button);
                tracker.last_event = Some(MouseLastEvent {
                    kind: MouseEventKind::Up,
                    col,
                    row,
                    button: Some(button),
                    scroll_dir: None,
                    mods,
                    ts_unix: now_unix(),
                });
            }
            WireEvent::Move { col, row, mods } => {
                tracker.record_position(col, row);
                tracker.last_event = Some(MouseLastEvent {
                    kind: MouseEventKind::Move,
                    col,
                    row,
                    button: None,
                    scroll_dir: None,
                    mods,
                    ts_unix: now_unix(),
                });
            }
            WireEvent::DragMove {
                col,
                row,
                button,
                mods,
            } => {
                tracker.record_position(col, row);
                tracker.last_event = Some(MouseLastEvent {
                    kind: MouseEventKind::DragMove,
                    col,
                    row,
                    button: Some(button),
                    scroll_dir: None,
                    mods,
                    ts_unix: now_unix(),
                });
            }
            WireEvent::Scroll {
                col,
                row,
                dir,
                mods,
            } => {
                // Scroll is position-independent in the agent's mental model;
                // don't move the synthetic cursor. last_event still records
                // the coords that went on the wire.
                tracker.last_event = Some(MouseLastEvent {
                    kind: MouseEventKind::Scroll,
                    col,
                    row,
                    button: None,
                    scroll_dir: Some(dir),
                    mods,
                    ts_unix: now_unix(),
                });
            }
        }
    }
}

fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn vt_mode_to_proto(mode: crate::emu::MouseProtocolMode) -> MouseMode {
    match mode {
        crate::emu::MouseProtocolMode::None => MouseMode::None,
        crate::emu::MouseProtocolMode::PressRelease => MouseMode::PressRelease,
        crate::emu::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
        crate::emu::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
    }
}

fn vt_encoding_to_proto(enc: crate::emu::MouseProtocolEncoding) -> MouseEncoding {
    match enc {
        crate::emu::MouseProtocolEncoding::Default => MouseEncoding::Default,
        crate::emu::MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
        crate::emu::MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
    }
}

fn pick_match(
    hits: &[crate::mouse::ScreenMatch],
    match_index: usize,
    label: &str,
) -> Result<(u16, u16), String> {
    if hits.is_empty() {
        return Err(format!("no match for {label} on visible screen"));
    }
    let chosen = hits.get(match_index).ok_or_else(|| {
        format!(
            "match-index {} out of range for {label} ({} match{})",
            match_index,
            hits.len(),
            if hits.len() == 1 { "" } else { "es" }
        )
    })?;
    Ok(chosen.center())
}

fn build_events<F>(
    action: &MouseAction,
    resolve: &F,
    mode: MouseMode,
) -> Result<Vec<WireEvent>, String>
where
    F: Fn(&MouseTarget) -> Result<(u16, u16), String>,
{
    use MouseAction::*;
    let mut out = Vec::new();
    match action {
        Click {
            target,
            button,
            mods,
            clicks,
        } => {
            let (col, row) = resolve(target)?;
            let n = (*clicks).max(1);
            for _ in 0..n {
                out.push(WireEvent::Down {
                    col,
                    row,
                    button: *button,
                    mods: *mods,
                });
                out.push(WireEvent::Up {
                    col,
                    row,
                    button: *button,
                    mods: *mods,
                });
            }
        }
        Down {
            target,
            button,
            mods,
        } => {
            let (col, row) = resolve(target)?;
            out.push(WireEvent::Down {
                col,
                row,
                button: *button,
                mods: *mods,
            });
        }
        Up {
            target,
            button,
            mods,
        } => {
            let (col, row) = resolve(target)?;
            out.push(WireEvent::Up {
                col,
                row,
                button: *button,
                mods: *mods,
            });
        }
        Move { target, mods } => {
            if matches!(mode, MouseMode::None | MouseMode::PressRelease) {
                return Err(format!(
                    "mouse mode {mode:?} does not report bare motion (need ButtonMotion or AnyMotion)"
                ));
            }
            let (col, row) = resolve(target)?;
            out.push(WireEvent::Move {
                col,
                row,
                mods: *mods,
            });
        }
        Drag {
            from,
            to,
            button,
            mods,
        } => {
            let (c1, r1) = resolve(from)?;
            let (c2, r2) = resolve(to)?;
            out.push(WireEvent::Down {
                col: c1,
                row: r1,
                button: *button,
                mods: *mods,
            });
            // Linearly interpolate intermediate cells so apps that track the path
            // (selection drags, panel dividers) see motion, not a teleport.
            for (col, row) in interpolate_path(c1, r1, c2, r2) {
                out.push(WireEvent::DragMove {
                    col,
                    row,
                    button: *button,
                    mods: *mods,
                });
            }
            out.push(WireEvent::Up {
                col: c2,
                row: r2,
                button: *button,
                mods: *mods,
            });
        }
        Scroll {
            target,
            dir,
            amount,
            mods,
        } => {
            let (col, row) = match target {
                Some(t) => resolve(t)?,
                None => (0, 0),
            };
            let n = (*amount).max(1);
            for _ in 0..n {
                out.push(WireEvent::Scroll {
                    col,
                    row,
                    dir: *dir,
                    mods: *mods,
                });
            }
        }
    }
    Ok(out)
}

/// Cells between (c1,r1) and (c2,r2) exclusive of both endpoints, using
/// Bresenham-style stepping so straight horizontal/vertical drags emit one
/// event per cell and diagonals stay roughly on the line.
fn interpolate_path(c1: u16, r1: u16, c2: u16, r2: u16) -> Vec<(u16, u16)> {
    let dx = (c2 as i32 - c1 as i32).abs();
    let dy = (r2 as i32 - r1 as i32).abs();
    let steps = dx.max(dy);
    if steps <= 1 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity((steps - 1) as usize);
    for i in 1..steps {
        let t = i as f64 / steps as f64;
        let col = (c1 as f64 + (c2 as f64 - c1 as f64) * t).round() as u16;
        let row = (r1 as f64 + (r2 as f64 - r1 as f64) * t).round() as u16;
        out.push((col, row));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::protocol::{MouseButton, ScrollDir};

    fn coords(col: u16, row: u16) -> MouseTarget {
        MouseTarget::Coords { col, row }
    }

    #[test]
    fn build_click_emits_down_up() {
        let action = MouseAction::Click {
            target: coords(5, 5),
            button: MouseButton::Left,
            mods: Default::default(),
            clicks: 1,
        };
        let resolve = |t: &MouseTarget| match t {
            MouseTarget::Coords { col, row } => Ok((*col, *row)),
            _ => Err("nope".into()),
        };
        let evs = build_events(&action, &resolve, MouseMode::PressRelease).unwrap();
        assert_eq!(evs.len(), 2);
        assert!(matches!(evs[0], WireEvent::Down { .. }));
        assert!(matches!(evs[1], WireEvent::Up { .. }));
    }

    #[test]
    fn build_double_click_clicks_2_emits_4_events() {
        let action = MouseAction::Click {
            target: coords(0, 0),
            button: MouseButton::Left,
            mods: Default::default(),
            clicks: 2,
        };
        let resolve = |t: &MouseTarget| match t {
            MouseTarget::Coords { col, row } => Ok((*col, *row)),
            _ => Err("nope".into()),
        };
        let evs = build_events(&action, &resolve, MouseMode::PressRelease).unwrap();
        assert_eq!(evs.len(), 4);
    }

    #[test]
    fn build_drag_emits_down_path_up() {
        let action = MouseAction::Drag {
            from: coords(0, 0),
            to: coords(5, 0),
            button: MouseButton::Left,
            mods: Default::default(),
        };
        let resolve = |t: &MouseTarget| match t {
            MouseTarget::Coords { col, row } => Ok((*col, *row)),
            _ => Err("nope".into()),
        };
        let evs = build_events(&action, &resolve, MouseMode::ButtonMotion).unwrap();
        // Down + 4 intermediate (cols 1..=4) + Up
        assert_eq!(evs.len(), 6);
        assert!(matches!(evs[0], WireEvent::Down { col: 0, row: 0, .. }));
        assert!(matches!(evs[5], WireEvent::Up { col: 5, row: 0, .. }));
        for ev in &evs[1..5] {
            assert!(matches!(ev, WireEvent::DragMove { .. }));
        }
    }

    #[test]
    fn build_move_rejected_when_mode_lacks_motion() {
        let action = MouseAction::Move {
            target: coords(0, 0),
            mods: Default::default(),
        };
        let resolve = |_: &MouseTarget| Ok((0, 0));
        let err = build_events(&action, &resolve, MouseMode::PressRelease).unwrap_err();
        assert!(err.contains("does not report bare motion"));
    }

    #[test]
    fn build_scroll_amount_replicates() {
        let action = MouseAction::Scroll {
            target: None,
            dir: ScrollDir::Down,
            amount: 5,
            mods: Default::default(),
        };
        let resolve = |_: &MouseTarget| Ok((0, 0));
        let evs = build_events(&action, &resolve, MouseMode::PressRelease).unwrap();
        assert_eq!(evs.len(), 5);
    }

    #[test]
    fn interpolate_horizontal() {
        let path = interpolate_path(0, 0, 5, 0);
        assert_eq!(path, vec![(1, 0), (2, 0), (3, 0), (4, 0)]);
    }

    #[test]
    fn interpolate_short_emits_nothing() {
        assert!(interpolate_path(0, 0, 1, 0).is_empty());
        assert!(interpolate_path(0, 0, 0, 0).is_empty());
    }

    fn ev_down(col: u16, row: u16, button: MouseButton) -> WireEvent {
        WireEvent::Down {
            col,
            row,
            button,
            mods: Default::default(),
        }
    }
    fn ev_up(col: u16, row: u16, button: MouseButton) -> WireEvent {
        WireEvent::Up {
            col,
            row,
            button,
            mods: Default::default(),
        }
    }

    #[test]
    fn tracker_down_records_position_and_held_button() {
        let mut t = crate::daemon::session::MouseTracker::default();
        update_tracker(&mut t, &[ev_down(10, 5, MouseButton::Left)]);
        assert_eq!(
            t.cursor,
            Some(crate::daemon::protocol::CursorPos { row: 5, col: 10 })
        );
        assert_eq!(t.buttons_held, vec![MouseButton::Left]);
        let last = t.last_event.as_ref().unwrap();
        assert_eq!(last.kind, MouseEventKind::Down);
        assert_eq!(last.col, 10);
        assert_eq!(last.row, 5);
    }

    #[test]
    fn tracker_up_releases_button_keeps_cursor() {
        let mut t = crate::daemon::session::MouseTracker::default();
        update_tracker(
            &mut t,
            &[
                ev_down(10, 5, MouseButton::Left),
                ev_up(11, 5, MouseButton::Left),
            ],
        );
        assert!(t.buttons_held.is_empty());
        assert_eq!(
            t.cursor,
            Some(crate::daemon::protocol::CursorPos { row: 5, col: 11 })
        );
    }

    #[test]
    fn tracker_click_leaves_no_held_buttons() {
        let mut t = crate::daemon::session::MouseTracker::default();
        update_tracker(
            &mut t,
            &[
                ev_down(0, 0, MouseButton::Left),
                ev_up(0, 0, MouseButton::Left),
            ],
        );
        assert!(t.buttons_held.is_empty());
    }

    #[test]
    fn tracker_drag_ends_clean() {
        let mut t = crate::daemon::session::MouseTracker::default();
        let evs = vec![
            ev_down(0, 0, MouseButton::Left),
            WireEvent::DragMove {
                col: 1,
                row: 0,
                button: MouseButton::Left,
                mods: Default::default(),
            },
            WireEvent::DragMove {
                col: 2,
                row: 0,
                button: MouseButton::Left,
                mods: Default::default(),
            },
            ev_up(3, 0, MouseButton::Left),
        ];
        update_tracker(&mut t, &evs);
        assert!(t.buttons_held.is_empty());
        assert_eq!(
            t.cursor,
            Some(crate::daemon::protocol::CursorPos { row: 0, col: 3 })
        );
        let last = t.last_event.as_ref().unwrap();
        assert_eq!(last.kind, MouseEventKind::Up);
    }

    #[test]
    fn tracker_two_buttons_held_in_order() {
        let mut t = crate::daemon::session::MouseTracker::default();
        update_tracker(
            &mut t,
            &[
                ev_down(0, 0, MouseButton::Left),
                ev_down(0, 0, MouseButton::Right),
            ],
        );
        assert_eq!(t.buttons_held, vec![MouseButton::Left, MouseButton::Right]);
        update_tracker(&mut t, &[ev_up(0, 0, MouseButton::Left)]);
        assert_eq!(t.buttons_held, vec![MouseButton::Right]);
    }

    #[test]
    fn tracker_double_down_does_not_dup() {
        let mut t = crate::daemon::session::MouseTracker::default();
        update_tracker(
            &mut t,
            &[
                ev_down(0, 0, MouseButton::Left),
                ev_down(1, 0, MouseButton::Left),
            ],
        );
        assert_eq!(t.buttons_held, vec![MouseButton::Left]);
    }

    #[test]
    fn tracker_scroll_does_not_move_cursor() {
        let mut t = crate::daemon::session::MouseTracker::default();
        update_tracker(&mut t, &[ev_down(20, 10, MouseButton::Left)]);
        update_tracker(&mut t, &[ev_up(20, 10, MouseButton::Left)]);
        let cursor_before = t.cursor;
        update_tracker(
            &mut t,
            &[WireEvent::Scroll {
                col: 0,
                row: 0,
                dir: ScrollDir::Down,
                mods: Default::default(),
            }],
        );
        assert_eq!(t.cursor, cursor_before);
        assert_eq!(t.last_event.as_ref().unwrap().kind, MouseEventKind::Scroll);
    }

    #[test]
    fn tracker_clamp_clears_cursor_when_out_of_bounds() {
        let mut t = crate::daemon::session::MouseTracker {
            cursor: Some(crate::daemon::protocol::CursorPos { row: 30, col: 90 }),
            ..Default::default()
        };
        t.clamp_to_size(&TermSize { cols: 80, rows: 24 });
        assert!(t.cursor.is_none());
    }

    #[test]
    fn tracker_clamp_keeps_cursor_when_in_bounds() {
        let mut t = crate::daemon::session::MouseTracker {
            cursor: Some(crate::daemon::protocol::CursorPos { row: 10, col: 50 }),
            ..Default::default()
        };
        t.clamp_to_size(&TermSize { cols: 80, rows: 24 });
        assert_eq!(
            t.cursor,
            Some(crate::daemon::protocol::CursorPos { row: 10, col: 50 })
        );
    }

    #[test]
    fn pick_match_disambiguation() {
        let hits = vec![
            crate::mouse::ScreenMatch {
                row: 0,
                col_start: 0,
                col_end: 3,
            },
            crate::mouse::ScreenMatch {
                row: 1,
                col_start: 4,
                col_end: 6,
            },
        ];
        assert_eq!(pick_match(&hits, 0, "x").unwrap(), (1, 0));
        assert_eq!(pick_match(&hits, 1, "x").unwrap(), (5, 1));
        assert!(pick_match(&hits, 5, "x").is_err());
        assert!(pick_match(&[], 0, "x").is_err());
    }
}
