use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};

use crate::daemon::protocol::{
    MouseAction, MouseEncoding, MouseMode, MouseTarget, Request, Response, SessionInfo, TermSize,
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

            Request::Mouse {
                name,
                action,
                force,
            } => self.handle_mouse(&name, action, force).await,

            Request::MouseState { name } => self.handle_mouse_state(&name).await,

            // Wait is handled directly in server.rs to avoid holding the manager lock
            Request::Wait { .. } => unreachable!("Wait should be handled in server.rs"),

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
    pub fn get_session_parser(&self, name: &str) -> Option<(Arc<Mutex<vt100::Parser>>, TermSize)> {
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
                }
            }
            None => Response::Error {
                message: format!("Session {name:?} not found"),
            },
        }
    }

    async fn handle_mouse(&mut self, name: &str, action: MouseAction, force: bool) -> Response {
        let session = match self.sessions.get(name) {
            Some(s) => s,
            None => {
                return Response::Error {
                    message: format!("Session {name:?} not found"),
                }
            }
        };

        let cols = session.size.cols;
        let rows = session.size.rows;

        // Snapshot mouse mode/encoding and the rendered screen text under one lock
        // so we can resolve text targets without races.
        let (mode, encoding, screen_rows) = {
            let parser = session.parser.lock().await;
            let screen = parser.screen();
            let mode = vt_mode_to_proto(screen.mouse_protocol_mode());
            let enc = vt_encoding_to_proto(screen.mouse_protocol_encoding());
            let mut text_rows = Vec::with_capacity(rows as usize);
            for r in 0..rows {
                let mut line = String::new();
                for c in 0..cols {
                    let cell = screen.cell(r, c).unwrap();
                    if cell.is_wide_continuation() {
                        continue;
                    }
                    let ch = cell.contents();
                    if ch.is_empty() {
                        line.push(' ');
                    } else {
                        line.push_str(&ch);
                    }
                }
                text_rows.push(line);
            }
            (mode, enc, text_rows)
        };

        if !force && mode == MouseMode::None {
            return Response::Error {
                message: format!(
                    "session {name:?} has not enabled mouse reporting (DECSET 1000/1002/1006). \
                     Use --force to send raw bytes anyway."
                ),
            };
        }

        // Resolve targets to coordinates.
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

        match session.write_bytes(&bytes) {
            Ok(()) => Response::Ok,
            Err(e) => Response::Error {
                message: format!("Mouse write failed: {e}"),
            },
        }
    }
}

fn vt_mode_to_proto(mode: vt100::MouseProtocolMode) -> MouseMode {
    match mode {
        vt100::MouseProtocolMode::None => MouseMode::None,
        vt100::MouseProtocolMode::Press => MouseMode::Press,
        vt100::MouseProtocolMode::PressRelease => MouseMode::PressRelease,
        vt100::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
        vt100::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
    }
}

fn vt_encoding_to_proto(enc: vt100::MouseProtocolEncoding) -> MouseEncoding {
    match enc {
        vt100::MouseProtocolEncoding::Default => MouseEncoding::Default,
        vt100::MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
        vt100::MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
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
            if matches!(
                mode,
                MouseMode::None | MouseMode::Press | MouseMode::PressRelease
            ) {
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
