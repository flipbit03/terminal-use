//! Terminal-emulator wrapper around `alacritty_terminal` (and its embedded
//! `vte::ansi` parser).
//!
//! Why this layer exists: tu started on the `vt100` crate, which doesn't
//! consume modern shell-integration escapes (OSC 133 semantic prompts, APC,
//! and friends) — they leak into cells and render as `^[…` artifacts. We
//! switched to alacritty's emulator which handles those silently.
//!
//! The public surface here intentionally mirrors what we previously consumed
//! from `vt100`:
//!
//! - `Parser::new(rows, cols, scrollback)`, `parser.process(&[u8])`,
//!   `parser.screen()` / `screen_mut()`.
//! - `Screen::cell(row, col)`, `cursor_position()`, `size()`, `set_size(...)`,
//!   `contents()` / `contents_formatted()`, plus mouse-mode introspection.
//! - `Cell::contents/fgcolor/bgcolor/bold/italic/underline/inverse/is_wide_continuation`.
//! - `Color::{Default, Idx, Rgb}`.
//!
//! That kept the rest of the codebase mostly untouched. Where alacritty's
//! data model differs (it has no "default" color sentinel — Named/Indexed/Spec
//! collapse onto each other), we map back to a 3-variant `Color` to preserve
//! the SGR-emission code in `daemon::session::screenshot_cells`.

use std::ops::Range;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell as AlacCell, Flags as AlacFlags};
use alacritty_terminal::term::test::TermSize as AlacSize;
use alacritty_terminal::term::{Config as AlacConfig, Term, TermMode};
use alacritty_terminal::vte::ansi;

/// Drop-in stand-in for `vt100::Color`. Identical semantics in the SGR codepath
/// in `daemon::session.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    fn from_alac(c: ansi::Color) -> Self {
        match c {
            ansi::Color::Named(named) => match named {
                // The "logical default" colors map back to Default so our
                // SGR-emission omits a foreground/background code (matching
                // vt100's behaviour and how a real terminal renders).
                ansi::NamedColor::Foreground | ansi::NamedColor::Background => Color::Default,
                other => Color::Idx(other as u8),
            },
            ansi::Color::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
            ansi::Color::Indexed(i) => Color::Idx(i),
        }
    }
}

/// Mouse reporting mode the inner app has DECSET'd. Alacritty does not
/// separately track X10 press-only mode — anything that turns on click
/// reporting is reported here as `PressRelease`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseProtocolMode {
    None,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

/// Wire encoding for mouse reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseProtocolEncoding {
    Default,
    Utf8,
    Sgr,
}

/// One snapshot cell, owned and `Send`-safe, mirroring vt100's `Cell` API.
#[derive(Debug, Clone)]
pub struct Cell {
    contents: String,
    fg: Color,
    bg: Color,
    bold: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
    is_wide_continuation: bool,
}

impl Cell {
    pub fn contents(&self) -> &str {
        &self.contents
    }
    pub fn fgcolor(&self) -> Color {
        self.fg
    }
    pub fn bgcolor(&self) -> Color {
        self.bg
    }
    pub fn bold(&self) -> bool {
        self.bold
    }
    pub fn italic(&self) -> bool {
        self.italic
    }
    pub fn underline(&self) -> bool {
        self.underline
    }
    pub fn inverse(&self) -> bool {
        self.inverse
    }
    pub fn is_wide_continuation(&self) -> bool {
        self.is_wide_continuation
    }
}

fn cell_from_alac(c: &AlacCell) -> Cell {
    let flags = c.flags;
    let inverse = flags.contains(AlacFlags::INVERSE);
    let bold = flags.contains(AlacFlags::BOLD);
    let italic = flags.contains(AlacFlags::ITALIC);
    let underline = flags.intersects(
        AlacFlags::UNDERLINE
            | AlacFlags::DOUBLE_UNDERLINE
            | AlacFlags::DOTTED_UNDERLINE
            | AlacFlags::DASHED_UNDERLINE
            | AlacFlags::UNDERCURL,
    );
    let is_wide_continuation = flags.contains(AlacFlags::WIDE_CHAR_SPACER);

    // A cell whose `c` is `' '` and which has no zero-width characters reads
    // as "empty" to the rest of tu (matches vt100's `cell.contents().is_empty()`
    // behaviour). Encode that by emitting an empty string.
    let mut contents = if c.c == ' ' && !flags.contains(AlacFlags::WIDE_CHAR) {
        String::new()
    } else {
        let mut s = String::new();
        s.push(c.c);
        s
    };
    if let Some(zw) = c.zerowidth() {
        for ch in zw {
            contents.push(*ch);
        }
    }

    Cell {
        contents,
        fg: Color::from_alac(c.fg),
        bg: Color::from_alac(c.bg),
        bold,
        italic,
        underline,
        inverse,
        is_wide_continuation,
    }
}

/// EventListener proxy that captures `PtyWrite` events into a shared buffer.
///
/// Why this matters: alacritty's `Term` does not write to the PTY itself —
/// when the inner application sends a query that needs a reply (Device
/// Attributes, cursor-position report, etc.), alacritty produces an
/// `Event::PtyWrite(reply)` and expects the listener to forward those bytes
/// to the PTY master. Dropping them silently is what made mc, vim, and other
/// curses apps freeze on startup waiting for terminal responses.
///
/// The buffer is `Arc<Mutex<…>>` so the proxy (which lives inside `Term` by
/// value) and `Parser::take_pending_writes` can access it from the same task
/// without ownership gymnastics. The mutex never sees contention because
/// the parent `Session` already serialises every `process()` + drain pair
/// behind its own tokio mutex.
#[derive(Default, Clone)]
struct CaptureProxy {
    pending: Arc<Mutex<Vec<u8>>>,
}

impl EventListener for CaptureProxy {
    fn send_event(&self, ev: Event) {
        if let Event::PtyWrite(bytes) = ev {
            if let Ok(mut buf) = self.pending.lock() {
                buf.extend_from_slice(bytes.as_bytes());
            }
        }
    }
}

/// Alacritty-backed terminal parser. Public surface mirrors the slice of
/// `vt100::Parser` we used.
pub struct Parser {
    term: Term<CaptureProxy>,
    processor: ansi::Processor,
    rows: u16,
    cols: u16,
    pending_writes: Arc<Mutex<Vec<u8>>>,
}

impl Parser {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        let size = AlacSize::new(cols as usize, rows as usize);
        let config = AlacConfig {
            scrolling_history: scrollback,
            ..Default::default()
        };
        let pending_writes: Arc<Mutex<Vec<u8>>> = Arc::default();
        let proxy = CaptureProxy {
            pending: pending_writes.clone(),
        };
        let term = Term::new(config, &size, proxy);
        Self {
            term,
            processor: ansi::Processor::new(),
            rows,
            cols,
            pending_writes,
        }
    }

    /// Feed PTY bytes through the vte parser into the alacritty terminal.
    pub fn process(&mut self, bytes: &[u8]) {
        self.processor.advance(&mut self.term, bytes);
    }

    /// Drain any bytes the terminal wants to write back to the PTY (replies
    /// to Device Attributes queries, cursor reports, etc.). Caller is
    /// responsible for actually writing these to the master fd.
    pub fn take_pending_writes(&mut self) -> Vec<u8> {
        match self.pending_writes.lock() {
            Ok(mut buf) => std::mem::take(&mut *buf),
            Err(_) => Vec::new(),
        }
    }

    pub fn screen(&self) -> Screen<'_> {
        Screen { parser: self }
    }

    pub fn screen_mut(&mut self) -> ScreenMut<'_> {
        ScreenMut { parser: self }
    }
}

/// Read-only view into the parser's terminal state.
pub struct Screen<'a> {
    parser: &'a Parser,
}

impl<'a> Screen<'a> {
    pub fn cell(&self, row: u16, col: u16) -> Option<Cell> {
        if row >= self.parser.rows || col >= self.parser.cols {
            return None;
        }
        let grid = self.parser.term.grid();
        let cell = &grid[Line(row as i32)][Column(col as usize)];
        Some(cell_from_alac(cell))
    }

    /// Visible-screen cursor position as `(row, col)`, both 0-based.
    /// Wraps the alacritty cursor to row 0 if it sits off-screen (this
    /// matches what vt100 returned and what `tu cursor` documents).
    pub fn cursor_position(&self) -> (u16, u16) {
        let p: Point = self.parser.term.grid().cursor.point;
        let row = p.line.0.max(0).min((self.parser.rows as i32) - 1) as u16;
        let col = p.column.0.min(self.parser.cols.saturating_sub(1) as usize) as u16;
        (row, col)
    }

    pub fn size(&self) -> (u16, u16) {
        (self.parser.rows, self.parser.cols)
    }

    /// Visible-screen text as one `String` per row, with one Unicode character
    /// per terminal column. Wide-char continuation cells are emitted as `' '`
    /// so `chars().count()` of any row equals the terminal's column count —
    /// downstream code that maps byte / char offsets to screen columns
    /// (text targeting, regex matching, wait-for-text) won't drift.
    pub fn text_rows(&self) -> Vec<String> {
        let (rows, cols) = self.size();
        let mut out = Vec::with_capacity(rows as usize);
        for r in 0..rows {
            let mut line = String::new();
            for c in 0..cols {
                let cell = self.cell(r, c);
                match cell {
                    Some(cell) if cell.is_wide_continuation() => line.push(' '),
                    Some(cell) if cell.contents().is_empty() => line.push(' '),
                    Some(cell) => line.push_str(cell.contents()),
                    None => line.push(' '),
                }
            }
            out.push(line);
        }
        out
    }

    /// Plain-text dump of visible screen + scrollback, line by line.
    pub fn contents(&self) -> String {
        let grid = self.parser.term.grid();
        let total = grid.total_lines() as i32;
        let screen_lines = grid.screen_lines() as i32;
        // History extends from -(history_size) to -1; visible from 0 to
        // screen_lines-1.
        let history_size = (total - screen_lines).max(0);
        let range: Range<i32> = -history_size..screen_lines;
        let mut out = String::new();
        for line in range {
            let mut buf = String::new();
            for col in 0..self.parser.cols as usize {
                let cell = &grid[Line(line)][Column(col)];
                if cell.flags.contains(AlacFlags::WIDE_CHAR_SPACER) {
                    continue;
                }
                if cell.c == ' ' {
                    buf.push(' ');
                } else {
                    buf.push(cell.c);
                    if let Some(zw) = cell.zerowidth() {
                        for ch in zw {
                            buf.push(*ch);
                        }
                    }
                }
            }
            // Trim trailing spaces; the consumer can re-pad if it cares.
            while buf.ends_with(' ') {
                buf.pop();
            }
            out.push_str(&buf);
            out.push('\n');
        }
        // Drop trailing newline if present so callers that join lines
        // don't get a double-newline at the end.
        if out.ends_with('\n') {
            out.pop();
        }
        out
    }

    /// Re-emit the visible screen as raw ANSI bytes — used by the PNG path
    /// to feed a fresh emulator on the client side.
    pub fn contents_formatted(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.parser.rows as usize * self.parser.cols as usize);
        // Reset and place cursor at home.
        out.extend_from_slice(b"\x1b[2J\x1b[H");
        let mut prev_fg = Color::Default;
        let mut prev_bg = Color::Default;
        let mut prev_bold = false;
        let mut prev_italic = false;
        let mut prev_underline = false;
        let mut prev_inverse = false;
        for row in 0..self.parser.rows {
            for col in 0..self.parser.cols {
                let Some(cell) = self.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation {
                    continue;
                }
                let fg = cell.fg;
                let bg = cell.bg;
                let bold = cell.bold;
                let italic = cell.italic;
                let underline = cell.underline;
                let inverse = cell.inverse;
                let attrs_changed = fg != prev_fg
                    || bg != prev_bg
                    || bold != prev_bold
                    || italic != prev_italic
                    || underline != prev_underline
                    || inverse != prev_inverse;
                if attrs_changed {
                    out.extend_from_slice(b"\x1b[0");
                    if bold {
                        out.extend_from_slice(b";1");
                    }
                    if italic {
                        out.extend_from_slice(b";3");
                    }
                    if underline {
                        out.extend_from_slice(b";4");
                    }
                    if inverse {
                        out.extend_from_slice(b";7");
                    }
                    push_fg_sgr(&mut out, fg);
                    push_bg_sgr(&mut out, bg);
                    out.push(b'm');
                    prev_fg = fg;
                    prev_bg = bg;
                    prev_bold = bold;
                    prev_italic = italic;
                    prev_underline = underline;
                    prev_inverse = inverse;
                }
                let s = cell.contents();
                if s.is_empty() {
                    out.push(b' ');
                } else {
                    out.extend_from_slice(s.as_bytes());
                }
            }
            if row + 1 < self.parser.rows {
                out.extend_from_slice(b"\r\n");
            }
        }
        out.extend_from_slice(b"\x1b[0m");
        out
    }

    pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
        let mode = self.parser.term.mode();
        // Inspect in order of strongest report set (AnyMotion implies the rest).
        if mode.contains(TermMode::MOUSE_MOTION) {
            MouseProtocolMode::AnyMotion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            MouseProtocolMode::ButtonMotion
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            // Alacritty doesn't separately track "report on press only"
            // (DECSET 9, the original X10 protocol) versus "press + release"
            // (DECSET 1000). Anything that enables MOUSE_REPORT_CLICK in
            // alacritty is press+release in practice — that's what every
            // modern app uses.
            MouseProtocolMode::PressRelease
        } else {
            MouseProtocolMode::None
        }
    }

    pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
        let mode = self.parser.term.mode();
        if mode.contains(TermMode::SGR_MOUSE) {
            MouseProtocolEncoding::Sgr
        } else if mode.contains(TermMode::UTF8_MOUSE) {
            MouseProtocolEncoding::Utf8
        } else {
            MouseProtocolEncoding::Default
        }
    }
}

pub struct ScreenMut<'a> {
    parser: &'a mut Parser,
}

impl<'a> ScreenMut<'a> {
    pub fn set_size(&mut self, rows: u16, cols: u16) {
        let size = AlacSize::new(cols as usize, rows as usize);
        self.parser.term.resize(size);
        self.parser.rows = rows;
        self.parser.cols = cols;
    }
}

fn push_fg_sgr(out: &mut Vec<u8>, color: Color) {
    match color {
        Color::Default => {}
        Color::Idx(i) => {
            if i < 8 {
                out.extend_from_slice(format!(";{}", 30 + i).as_bytes());
            } else if i < 16 {
                out.extend_from_slice(format!(";{}", 90 + i - 8).as_bytes());
            } else {
                out.extend_from_slice(format!(";38;5;{i}").as_bytes());
            }
        }
        Color::Rgb(r, g, b) => {
            out.extend_from_slice(format!(";38;2;{r};{g};{b}").as_bytes());
        }
    }
}

fn push_bg_sgr(out: &mut Vec<u8>, color: Color) {
    match color {
        Color::Default => {}
        Color::Idx(i) => {
            if i < 8 {
                out.extend_from_slice(format!(";{}", 40 + i).as_bytes());
            } else if i < 16 {
                out.extend_from_slice(format!(";{}", 100 + i - 8).as_bytes());
            } else {
                out.extend_from_slice(format!(";48;5;{i}").as_bytes());
            }
        }
        Color::Rgb(r, g, b) => {
            out.extend_from_slice(format!(";48;2;{r};{g};{b}").as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_reads_ascii() {
        let mut p = Parser::new(2, 10, 0);
        p.process(b"hi");
        let s = p.screen();
        let c = s.cell(0, 0).unwrap();
        assert_eq!(c.contents(), "h");
        let c = s.cell(0, 1).unwrap();
        assert_eq!(c.contents(), "i");
    }

    #[test]
    fn sgr_red_foreground_round_trip() {
        let mut p = Parser::new(2, 10, 0);
        p.process(b"\x1b[31mA");
        let c = p.screen().cell(0, 0).unwrap();
        assert!(matches!(c.fgcolor(), Color::Idx(1)));
        assert_eq!(c.contents(), "A");
    }

    #[test]
    fn osc_133_silently_consumed_no_caret_artifact() {
        // OSC 133;A (semantic prompt mark, terminated by ST = ESC \).
        // Followed by visible text. The ESC should NOT end up in any cell.
        let mut p = Parser::new(2, 10, 0);
        p.process(b"\x1b]133;A\x1b\\hello");
        let mut buf = String::new();
        for col in 0..10 {
            let cell = p.screen().cell(0, col).unwrap();
            if cell.contents().is_empty() {
                buf.push(' ');
            } else {
                buf.push_str(cell.contents());
            }
        }
        assert!(
            !buf.contains('\u{1b}'),
            "row contained an unparsed ESC byte: {buf:?}"
        );
        assert!(
            buf.starts_with("hello"),
            "expected hello prefix, got {buf:?}"
        );
    }

    #[test]
    fn apc_silently_consumed() {
        let mut p = Parser::new(2, 10, 0);
        // APC: ESC _ <payload> ESC \\
        p.process(b"\x1b_xyz\x1b\\hello");
        let mut buf = String::new();
        for col in 0..10 {
            let cell = p.screen().cell(0, col).unwrap();
            if cell.contents().is_empty() {
                buf.push(' ');
            } else {
                buf.push_str(cell.contents());
            }
        }
        assert!(
            !buf.contains('\u{1b}') && !buf.contains('_'),
            "APC bled through into cells: {buf:?}"
        );
        assert!(buf.starts_with("hello"));
    }

    #[test]
    fn mouse_decset_1000_then_1006() {
        let mut p = Parser::new(2, 10, 0);
        p.process(b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(
            p.screen().mouse_protocol_mode(),
            MouseProtocolMode::PressRelease
        );
        assert_eq!(
            p.screen().mouse_protocol_encoding(),
            MouseProtocolEncoding::Sgr
        );
    }

    #[test]
    fn cursor_position_after_writes() {
        let mut p = Parser::new(3, 10, 0);
        p.process(b"abc");
        assert_eq!(p.screen().cursor_position(), (0, 3));
    }

    #[test]
    fn resize_updates_dimensions() {
        let mut p = Parser::new(2, 4, 0);
        p.process(b"hi");
        p.screen_mut().set_size(4, 8);
        assert_eq!(p.screen().size(), (4, 8));
    }

    #[test]
    fn alt_screen_content_is_reachable_via_cell() {
        let mut p = Parser::new(5, 20, 0);
        // Mimic what curses apps (mc, vim) do on startup: switch to alt
        // screen, clear, then draw. Reading cells must reflect what's on
        // the alt screen, not the primary one.
        p.process(b"\x1b[?1049h\x1b[2J\x1b[Hhello world");
        let s = p.screen();
        let mut row0 = String::new();
        for col in 0..20 {
            let c = s.cell(0, col).unwrap();
            if c.contents().is_empty() {
                row0.push(' ');
            } else {
                row0.push_str(c.contents());
            }
        }
        assert!(
            row0.starts_with("hello world"),
            "expected alt-screen content visible, got {row0:?}"
        );
    }

    #[test]
    fn da_query_produces_pty_writeback() {
        let mut p = Parser::new(2, 10, 0);
        // Primary Device Attributes query.
        p.process(b"\x1b[c");
        let pending = p.take_pending_writes();
        assert!(
            !pending.is_empty(),
            "expected DA reply queued; got nothing — alacritty proxy not capturing PtyWrite"
        );
    }
}
