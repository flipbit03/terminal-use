use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};

use crate::daemon::protocol::{Request, Response, SessionInfo, TermSize};
use crate::daemon::session::Session;

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
    pub fn get_session_parser(
        &self,
        name: &str,
    ) -> Option<(Arc<Mutex<vt100::Parser>>, TermSize)> {
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

}
