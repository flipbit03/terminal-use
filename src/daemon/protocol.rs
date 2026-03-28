use serde::{Deserialize, Serialize};

/// Size specification for a terminal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TermSize {
    fn default() -> Self {
        Self {
            cols: 120,
            rows: 40,
        }
    }
}

/// Requests from CLI → Daemon.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    Run {
        command: String,
        args: Vec<String>,
        name: Option<String>,
        size: TermSize,
        scrollback: usize,
        env: Vec<(String, String)>,
        cwd: Option<String>,
        term: String,
        shell: bool,
    },
    Kill {
        name: String,
    },
    List,
    Status {
        name: String,
    },
    Screenshot {
        name: String,
    },
    /// Request the raw ANSI-formatted screen bytes for client-side image rendering.
    ScreenshotAnsi {
        name: String,
    },
    /// Request pre-rendered ANSI row strings (one per visible row).
    ScreenshotCells {
        name: String,
    },
    Cursor {
        name: String,
    },
    Scrollback {
        name: String,
        lines: Option<usize>,
    },
    Type {
        name: String,
        text: String,
    },
    Press {
        name: String,
        keys: Vec<u8>,
    },
    Paste {
        name: String,
        text: String,
    },
    Resize {
        name: String,
        size: TermSize,
    },
    Wait {
        name: String,
        stable_ms: Option<u64>,
        text_pattern: Option<String>,
        timeout_ms: u64,
    },
    Shutdown,
}

/// A session's status info.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub pid: u32,
    pub alive: bool,
    pub exit_code: Option<i32>,
    pub size: TermSize,
}

/// Cursor position.
#[derive(Debug, Serialize, Deserialize)]
pub struct CursorPos {
    pub row: u16,
    pub col: u16,
}

/// Responses from Daemon → CLI.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    Ok,
    SessionCreated {
        name: String,
        pid: u32,
    },
    SessionList {
        sessions: Vec<SessionInfo>,
    },
    Status {
        #[serde(flatten)]
        info: SessionInfo,
    },
    Screenshot {
        content: String,
        rows: u16,
        cols: u16,
        cursor: CursorPos,
    },
    Cursor {
        #[serde(flatten)]
        pos: CursorPos,
    },
    /// Raw ANSI screen bytes for client-side vt100 replay and image rendering.
    ///
    /// The payload is base64-encoded because the ANSI stream contains arbitrary bytes
    /// (escape sequences, control characters) that are not valid UTF-8 and would break
    /// JSON serialization.
    ScreenshotAnsi {
        content_b64: String,
        rows: u16,
        cols: u16,
    },
    ScreenshotCells {
        /// Each row is a vector of ANSI-rendered strings (one per row, already SGR-formatted).
        rows_ansi: Vec<String>,
        rows: u16,
        cols: u16,
    },
    Scrollback {
        content: String,
    },
    Error {
        message: String,
    },
}
