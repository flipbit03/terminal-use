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
    Mouse {
        name: String,
        action: MouseAction,
        force: bool,
    },
    MouseState {
        name: String,
    },
    Wait {
        name: String,
        stable_ms: Option<u64>,
        text_pattern: Option<String>,
        timeout_ms: u64,
    },
    Shutdown,
}

/// Mouse button to send.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Modifier keys held during a mouse event.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MouseMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// Scroll direction for the wheel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScrollDir {
    Up,
    Down,
    Left,
    Right,
}

/// How to locate the cell to click.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum MouseTarget {
    /// Absolute cell coordinates (0-based).
    Coords { col: u16, row: u16 },
    /// Find first (or nth) literal-text match on the visible screen.
    Text { needle: String, match_index: usize },
    /// Find first (or nth) regex match on the visible screen.
    Regex { pattern: String, match_index: usize },
}

/// One mouse operation. Compound ops (click, drag) are expanded inside the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum MouseAction {
    Click {
        target: MouseTarget,
        button: MouseButton,
        mods: MouseMods,
        clicks: u32,
    },
    Down {
        target: MouseTarget,
        button: MouseButton,
        mods: MouseMods,
    },
    Up {
        target: MouseTarget,
        button: MouseButton,
        mods: MouseMods,
    },
    Move {
        target: MouseTarget,
        mods: MouseMods,
    },
    Drag {
        from: MouseTarget,
        to: MouseTarget,
        button: MouseButton,
        mods: MouseMods,
    },
    Scroll {
        target: Option<MouseTarget>,
        dir: ScrollDir,
        amount: u32,
        mods: MouseMods,
    },
}

/// Mouse-mode introspection: what the inner app has DECSET'd.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MouseMode {
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MouseEncoding {
    Default,
    Utf8,
    Sgr,
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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
        /// Synthetic mouse cursor position (None until first mouse event, or after
        /// resize-out-of-bounds clear). Renderers can paint an overlay at this cell.
        mouse_cursor: Option<CursorPos>,
        /// True when at least one mouse button is currently held (Down without
        /// matching Up). Renderers use this to switch the cursor between an outline
        /// (idle) and a filled block (drag/press in progress).
        mouse_held: bool,
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
    MouseState {
        mode: MouseMode,
        encoding: MouseEncoding,
        size: TermSize,
        cursor: Option<CursorPos>,
        buttons_held: Vec<MouseButton>,
        last_event: Option<MouseLastEvent>,
    },
    Error {
        message: String,
    },
}

/// Kind of the most recent mouse event tu emitted (for `mouse state`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MouseEventKind {
    Down,
    Up,
    Move,
    DragMove,
    Scroll,
}

/// Snapshot of the last mouse event written to the PTY for this session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MouseLastEvent {
    pub kind: MouseEventKind,
    pub col: u16,
    pub row: u16,
    /// Some for Down/Up/DragMove; None for Move (no button) and Scroll
    /// (direction is in `scroll_dir` instead).
    pub button: Option<MouseButton>,
    pub scroll_dir: Option<ScrollDir>,
    pub mods: MouseMods,
    /// Unix-epoch seconds when tu emitted the event.
    pub ts_unix: u64,
}
