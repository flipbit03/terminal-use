use std::io::{self, Read, Write};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::daemon::protocol::{CursorPos, Request, Response};
use crate::daemon::server::{ensure_daemon, send_request};

/// Run the attach live viewer.
pub async fn run(initial_name: String) -> Result<()> {
    ensure_daemon()?;

    // Enter raw mode + alternate screen
    let mut tty = RawTerminal::enter()?;

    // Wait for at least one session to appear
    loop {
        let sessions = get_session_names().await.unwrap_or_default();
        if !sessions.is_empty() {
            break;
        }
        draw_waiting_screen()?;
        if let Some(Key::Quit) = tty.read_key(Duration::from_millis(250))? {
            drop(tty);
            return Ok(());
        }
    }

    // Find initial session index
    let sessions = get_session_names().await.unwrap_or_default();
    let mut current_idx = sessions
        .iter()
        .position(|s| s == &initial_name)
        .unwrap_or(0);

    let result = run_loop(&mut tty, &mut current_idx).await;

    // Always restore terminal
    drop(tty);

    result
}

/// Frame interval for the live monitor. ~30fps; chosen high enough to feel
/// fluid for drag/scroll visualization but not so high that we burn CPU.
/// Monitor sessions are interactive and short-lived, so the extra work is fine.
const FRAME_INTERVAL_MS: u64 = 33;

async fn run_loop(tty: &mut RawTerminal, current_idx: &mut usize) -> Result<()> {
    let mut last_rows: Option<Vec<String>> = None;
    let mut last_term_size = get_terminal_size();
    let mut last_fetch = std::time::Instant::now() - Duration::from_secs(10); // force immediate first fetch
    let mut last_change = std::time::Instant::now();
    // True when the next emission needs to wipe the alt screen first
    // (startup, session switch, error, resize). Cleared after the next
    // successful emit.
    let mut needs_clear = true;
    // Previously-emitted frame, kept for row-by-row diff. Each frame is built
    // into a fresh `Vec<String>` and compared to this one; rows whose string
    // matches the prior frame's are skipped, leaving the terminal undisturbed.
    // This is the bulk of the flicker fix — at 30fps with a mostly-static
    // inner app, a typical tick writes the status line and nothing else.
    let mut prev_frame: Option<Vec<String>> = None;

    let fetch_interval = Duration::from_millis(FRAME_INTERVAL_MS);

    loop {
        // Detect terminal resize → clear screen + force redraw
        let term_size = get_terminal_size();
        if term_size != last_term_size {
            last_term_size = term_size;
            last_rows = None;
            needs_clear = true;
            prev_frame = None;
            last_fetch = std::time::Instant::now() - fetch_interval; // force refetch
        }

        // Fetch + redraw at ~30fps.
        if last_fetch.elapsed() >= fetch_interval {
            last_fetch = std::time::Instant::now();

            let sessions = get_session_names().await.unwrap_or_default();
            if sessions.is_empty() {
                draw_waiting_screen()?;
                needs_clear = true;
                prev_frame = None;
                match tty.read_key(Duration::from_millis(FRAME_INTERVAL_MS))? {
                    Some(Key::Quit) => break,
                    _ => continue,
                }
            }
            if *current_idx >= sessions.len() {
                *current_idx = sessions.len() - 1;
            }

            let session_name = &sessions[*current_idx];

            match send_request(&Request::ScreenshotCells {
                name: session_name.clone(),
            })
            .await
            {
                Ok(Response::ScreenshotCells {
                    rows_ansi,
                    rows,
                    cols,
                    mouse_cursor,
                    mouse_held,
                }) => {
                    let changed = last_rows.as_ref() != Some(&rows_ansi);
                    if changed {
                        last_change = std::time::Instant::now();
                        last_rows = Some(rows_ansi.clone());
                    }
                    let new_frame = build_frame_strings(
                        &sessions,
                        *current_idx,
                        &rows_ansi,
                        rows,
                        cols,
                        term_size,
                        last_change.elapsed(),
                        mouse_cursor,
                        mouse_held,
                    );
                    emit_frame_diff(
                        needs_clear,
                        prev_frame.as_deref(),
                        &new_frame,
                        &sessions[*current_idx],
                    )?;
                    needs_clear = false;
                    prev_frame = Some(new_frame);
                }
                Ok(Response::Error { message: _ }) => {
                    last_rows = None;
                    needs_clear = true;
                    prev_frame = None;
                }
                _ => {}
            }
        }

        // Poll keys with a short timeout so the frame loop stays responsive.
        // The wake-up effectively caps redraws at FRAME_INTERVAL_MS.
        match tty.read_key(Duration::from_millis(FRAME_INTERVAL_MS))? {
            Some(Key::Quit) => break,
            Some(Key::Left) if *current_idx > 0 => {
                *current_idx -= 1;
                last_rows = None;
                needs_clear = true;
                prev_frame = None;
                last_fetch = std::time::Instant::now() - fetch_interval; // force refetch
            }
            Some(Key::Right) => {
                let sessions = get_session_names().await.unwrap_or_default();
                if *current_idx + 1 < sessions.len() {
                    *current_idx += 1;
                    last_rows = None;
                    needs_clear = true;
                    prev_frame = None;
                    last_fetch = std::time::Instant::now() - fetch_interval;
                }
            }
            Some(Key::Left) | None => {}
        }
    }

    Ok(())
}

async fn get_session_names() -> Result<Vec<String>> {
    match send_request(&Request::List).await? {
        Response::SessionList { sessions } => Ok(sessions.into_iter().map(|s| s.name).collect()),
        _ => Ok(vec![]),
    }
}

fn draw_waiting_screen() -> Result<()> {
    let (cols, rows) = get_terminal_size();
    let mut out = io::stdout().lock();

    write!(out, "\x1b[H\x1b[2J")?;

    let line1 = "terminal-use";
    let line2 = "Waiting for sessions...";
    let line3 = "Ctrl+C to quit";
    let box_width = 32;
    let pad_x = (cols as usize).saturating_sub(box_width) / 2;
    // saturating_sub guards against the overflow that would happen on a
    // pathologically tiny terminal (rows < 4).
    let mid_row = (rows / 2).saturating_sub(2).max(1);
    let p = " ".repeat(pad_x);

    write!(
        out,
        "\x1b[{};1H{p}\x1b[90m┌{}┐\x1b[0m",
        mid_row,
        "─".repeat(box_width - 2)
    )?;
    write!(
        out,
        "\x1b[{};1H{p}\x1b[90m│\x1b[0m\x1b[1m{:^w$}\x1b[0m\x1b[90m│\x1b[0m",
        mid_row + 1,
        line1,
        w = box_width - 2
    )?;
    write!(
        out,
        "\x1b[{};1H{p}\x1b[90m│{:^w$}│\x1b[0m",
        mid_row + 2,
        line2,
        w = box_width - 2
    )?;
    write!(
        out,
        "\x1b[{};1H{p}\x1b[90m│{:^w$}│\x1b[0m",
        mid_row + 3,
        "",
        w = box_width - 2
    )?;
    write!(
        out,
        "\x1b[{};1H{p}\x1b[90m│\x1b[2m{:^w$}\x1b[0m\x1b[90m│\x1b[0m",
        mid_row + 4,
        line3,
        w = box_width - 2
    )?;
    write!(
        out,
        "\x1b[{};1H{p}\x1b[90m└{}┘\x1b[0m",
        mid_row + 5,
        "─".repeat(box_width - 2)
    )?;

    out.flush()?;
    Ok(())
}

fn get_terminal_size() -> (u16, u16) {
    // (cols, rows)
    unsafe {
        let mut ws: nix::libc::winsize = std::mem::zeroed();
        if nix::libc::ioctl(1, nix::libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            (ws.ws_col, ws.ws_row)
        } else {
            (80, 24)
        }
    }
}

fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}

/// Render the current frame as a vector of ANSI strings, one per visible
/// terminal row. Index 0 corresponds to terminal row 1.
///
/// The cursor overlay (when set) is appended to the cursor's row string as a
/// trailing `CSI row;col H <glyph>` sequence. That way the diff naturally
/// catches cursor movement: when the cursor moves, both the row it left and
/// the row it entered get re-emitted, but no other rows do.
#[allow(clippy::too_many_arguments)]
fn build_frame_strings(
    sessions: &[String],
    active_idx: usize,
    rows_ansi: &[String],
    sess_rows: u16,
    sess_cols: u16,
    term_size: (u16, u16),
    since_last_change: Duration,
    mouse_cursor: Option<CursorPos>,
    mouse_held: bool,
) -> Vec<String> {
    let (term_cols, term_rows) = term_size;

    let frame_width = sess_cols as usize + 2;
    let tcols = term_cols as usize;
    let cropped_right = tcols < frame_width;

    // Layout: status(1) + [tab bar(1)] + top border(1) + content + bottom border(1)
    let header_rows = if sessions.len() > 1 { 3u16 } else { 2u16 };
    let available_content_rows = term_rows.saturating_sub(header_rows + 1);
    let content_rows_to_show = sess_rows.min(available_content_rows);
    let cropped_bottom = content_rows_to_show < sess_rows;

    let mut frame: Vec<String> = Vec::with_capacity(term_rows as usize);

    // Row 1: status bar
    let elapsed = format_elapsed(since_last_change);
    let status = if sessions.len() > 1 {
        format!("terminal-use monitor · last change {elapsed} · ← → switch · Ctrl+C detach")
    } else {
        format!("terminal-use monitor · last change {elapsed} · Ctrl+C detach")
    };
    frame.push(format!("\x1b[90m{status}\x1b[0m"));

    // Tab bar
    if sessions.len() > 1 {
        frame.push(build_tab_bar(sessions, active_idx));
    }

    // Top border
    {
        let title = format!(" {} [{}x{}] ", &sessions[active_idx], sess_cols, sess_rows);
        let prefix_width = 2 + title.len();
        let line = if cropped_right {
            let dash_space = tcols.saturating_sub(prefix_width);
            let (dashes, suffix) = if dash_space > 3 {
                ("─".repeat(dash_space - 3), "···")
            } else {
                ("─".repeat(dash_space), "")
            };
            format!("\x1b[90m┌─\x1b[0m\x1b[1m{title}\x1b[0m\x1b[90m{dashes}{suffix}\x1b[0m")
        } else {
            let dashes = "─".repeat(frame_width.saturating_sub(prefix_width + 1));
            format!("\x1b[90m┌─\x1b[0m\x1b[1m{title}\x1b[0m\x1b[90m{dashes}┐\x1b[0m")
        };
        frame.push(line);
    }

    // Content rows
    let fade_start = if cropped_bottom {
        content_rows_to_show.saturating_sub(3) as usize
    } else {
        usize::MAX
    };

    for r in 0..content_rows_to_show as usize {
        let line = rows_ansi.get(r).map(|s| s.as_str()).unwrap_or("");
        let left_border = if r >= fade_start { "·" } else { "│" };

        let mut row_str = if cropped_right {
            // Without truncation, the daemon's `sess_cols` visible chars
            // would exceed the user's terminal width and autowrap onto the
            // next row, scrambling the display. Reserve 1 col for the left
            // border + 1 col of breathing room on the right so the clip
            // doesn't sit flush against the terminal edge.
            let max_visible = tcols.saturating_sub(2);
            let clipped = truncate_ansi_visible(line, max_visible);
            format!("\x1b[90m{left_border}\x1b[0m{clipped}\x1b[0m")
        } else {
            let right_border = if r >= fade_start { "·" } else { "│" };
            format!(
                "\x1b[90m{left_border}\x1b[0m{line}\x1b[0m\x1b[{col}G\x1b[90m{right_border}\x1b[0m",
                col = frame_width,
            )
        };

        // Cursor overlay: appended as a position-and-paint trailer, so the
        // diff naturally re-emits this row when the cursor moves on/off it
        // and skips it when the cursor stays put.
        if let Some(cursor) = mouse_cursor {
            if cursor.row as usize == r && cursor.col < sess_cols {
                let term_row = frame.len() as u16 + 1; // 1-indexed terminal row
                let term_col = 2 + cursor.col; // left border = col 1; content starts at col 2
                let max_col = if term_cols >= 2 { term_cols } else { 1 };
                if term_col <= max_col {
                    row_str.push_str(&format!(
                        "\x1b[{term_row};{term_col}H{}\x1b[0m",
                        mouse_cursor_glyph(mouse_held)
                    ));
                }
            }
        }

        frame.push(row_str);
    }

    // Bottom border (only if not cropped bottom)
    if !cropped_bottom {
        let line = if cropped_right {
            let dash_space = tcols.saturating_sub(1);
            let (dashes, suffix) = if dash_space > 3 {
                ("─".repeat(dash_space - 3), "···")
            } else {
                ("─".repeat(dash_space), "")
            };
            format!("\x1b[90m└{dashes}{suffix}\x1b[0m")
        } else {
            let dashes = "─".repeat(frame_width.saturating_sub(2));
            format!("\x1b[90m└{dashes}┘\x1b[0m")
        };
        frame.push(line);
    }

    frame
}

/// Truncate an ANSI-decorated line to at most `max_visible` printable
/// characters, copying SGR / OSC / other escape sequences verbatim. Used
/// when the user's terminal is narrower than the session frame: without
/// this, the daemon's `sess_cols` visible chars on each row would exceed
/// the user's terminal width and autowrap onto the next row, scrambling
/// neighbouring rows on every frame.
///
/// Visible-char counting is per Unicode scalar (one char = one column).
/// That's accurate for ASCII / Latin / box-drawing content. CJK wide
/// chars would over-count by one column each, but no TUIs we care about
/// in this code path emit them.
fn truncate_ansi_visible(line: &str, max_visible: usize) -> String {
    if max_visible == 0 {
        return String::new();
    }
    let mut out = String::with_capacity(line.len());
    let mut visible = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Copy the escape sequence verbatim.
            out.push(c);
            let Some(&next) = chars.peek() else { break };
            chars.next();
            out.push(next);
            match next {
                // CSI: parameters (0x30-0x3F) + intermediates (0x20-0x2F)
                // + final byte (0x40-0x7E).
                '[' => {
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        out.push(p);
                        let b = p as u32;
                        if (0x40..=0x7E).contains(&b) {
                            break;
                        }
                    }
                }
                // OSC: terminate on BEL (0x07) or ST (ESC \).
                ']' => {
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        out.push(p);
                        if p == '\x07' {
                            break;
                        }
                        if p == '\x1b' {
                            if let Some(&q) = chars.peek() {
                                chars.next();
                                out.push(q);
                                if q == '\\' {
                                    break;
                                }
                            }
                        }
                    }
                }
                // Other 2-char escapes (charset designators, app keypad,
                // etc.) — already wrote both bytes.
                _ => {}
            }
        } else {
            if visible >= max_visible {
                break;
            }
            out.push(c);
            visible += 1;
        }
    }
    out
}

/// SGR-wrapped glyph for tu's synthetic mouse cursor. Idle = magenta `△`;
/// held = bright-white `△` on a magenta cell.
fn mouse_cursor_glyph(held: bool) -> &'static str {
    if held {
        "\x1b[1;48;5;201;97m△"
    } else {
        "\x1b[1;38;5;201m△"
    }
}

/// Emit only the rows that changed vs the previously-emitted frame.
///
/// `needs_clear` (set on startup / session switch / error / resize) wipes
/// the alt screen first and forces a full redraw, regardless of `prev`.
///
/// Note: when no diff applies (every tick, the same status line, the same
/// content rows), this writes nothing — the user's terminal is left alone.
/// That's what kills the flicker compared to the full-repaint version.
fn emit_frame_diff(
    needs_clear: bool,
    prev: Option<&[String]>,
    new: &[String],
    session_name: &str,
) -> Result<()> {
    let mut out = io::stdout().lock();

    if needs_clear {
        write!(out, "\x1b[2J\x1b[H")?;
        write!(out, "\x1b]0;tu monitor: {session_name}\x07")?;
    }

    let prev_len = if needs_clear {
        // After a wipe, every row in `new` differs from "the screen" (which
        // is now empty). Treat prev as None for the diff loop.
        0
    } else {
        prev.map(|p| p.len()).unwrap_or(0)
    };

    for (i, line) in new.iter().enumerate() {
        let unchanged = !needs_clear
            && prev
                .and_then(|p| p.get(i))
                .map(|s| s == line)
                .unwrap_or(false);
        if unchanged {
            continue;
        }
        let row = i + 1;
        write!(out, "\x1b[{row};1H\x1b[2K{line}")?;
    }

    // If the new frame is shorter than the previous, clear what's beyond.
    if new.len() < prev_len {
        write!(out, "\x1b[{};1H\x1b[J", new.len() + 1)?;
    }

    out.flush()?;
    Ok(())
}

fn build_tab_bar(sessions: &[String], active_idx: usize) -> String {
    let mut bar = String::new();
    for (i, name) in sessions.iter().enumerate() {
        if i > 0 {
            bar.push_str("\x1b[90m │ \x1b[0m");
        }
        if i == active_idx {
            // Active: bold + inverse
            bar.push_str(&format!("\x1b[1;7m {} \x1b[0m", name));
        } else {
            // Inactive: dim
            bar.push_str(&format!("\x1b[2m {} \x1b[0m", name));
        }
    }
    bar
}

// --- Raw terminal handling ---

enum Key {
    Quit,
    Left,
    Right,
}

struct RawTerminal {
    original_termios: nix::sys::termios::Termios,
}

impl RawTerminal {
    fn enter() -> Result<Self> {
        use nix::sys::termios::{self, InputFlags, LocalFlags};

        if !std::io::IsTerminal::is_terminal(&io::stdin()) {
            anyhow::bail!("monitor requires a real terminal (TTY)");
        }

        let original = termios::tcgetattr(io::stdin()).context("tcgetattr")?;

        let mut raw = original.clone();
        // Disable canonical mode, echo, and signal generation
        raw.local_flags &= !(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG);
        // Disable input processing
        raw.input_flags &= !(InputFlags::IXON | InputFlags::ICRNL);
        // Set VMIN=0, VTIME=0 for non-blocking reads
        raw.control_chars[nix::sys::termios::SpecialCharacterIndices::VMIN as usize] = 0;
        raw.control_chars[nix::sys::termios::SpecialCharacterIndices::VTIME as usize] = 0;

        termios::tcsetattr(io::stdin(), termios::SetArg::TCSANOW, &raw).context("tcsetattr raw")?;

        // Enter alternate screen + hide cursor.
        print!("\x1b[?1049h\x1b[?25l");
        io::stdout().flush()?;

        Ok(Self {
            original_termios: original,
        })
    }

    fn read_key(&self, timeout: Duration) -> Result<Option<Key>> {
        use std::os::fd::AsRawFd;

        let stdin_fd = io::stdin().as_raw_fd();

        // Use poll to wait for input with timeout
        let mut pollfd = nix::poll::PollFd::new(
            unsafe { std::os::fd::BorrowedFd::borrow_raw(stdin_fd) },
            nix::poll::PollFlags::POLLIN,
        );
        let timeout_ms = timeout.as_millis() as u16;
        let ready = nix::poll::poll(std::slice::from_mut(&mut pollfd), timeout_ms).unwrap_or(0);

        if ready == 0 {
            return Ok(None);
        }

        let mut buf = [0u8; 8];
        let n = io::stdin().lock().read(&mut buf).unwrap_or(0);
        if n == 0 {
            return Ok(None);
        }

        // Ctrl+C = 0x03, q = 0x71
        if buf[0] == 0x03 || buf[0] == b'q' {
            return Ok(Some(Key::Quit));
        }

        // Arrow keys: \x1b [ C (right), \x1b [ D (left)
        // Also SS3: \x1b O C / \x1b O D
        if n >= 3 && buf[0] == 0x1b {
            if (buf[1] == b'[' || buf[1] == b'O') && buf[2] == b'C' {
                return Ok(Some(Key::Right));
            }
            if (buf[1] == b'[' || buf[1] == b'O') && buf[2] == b'D' {
                return Ok(Some(Key::Left));
            }
        }

        Ok(None)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        // Restore terminal: show cursor, leave alt screen.
        print!("\x1b[?25h\x1b[?1049l");
        let _ = io::stdout().flush();

        // Restore original termios
        let _ = nix::sys::termios::tcsetattr(
            io::stdin(),
            nix::sys::termios::SetArg::TCSANOW,
            &self.original_termios,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::truncate_ansi_visible;

    #[test]
    fn truncate_passes_short_lines_through_unchanged() {
        let line = "\x1b[31mhello\x1b[0m";
        assert_eq!(truncate_ansi_visible(line, 10), line);
    }

    #[test]
    fn truncate_clips_visible_chars_only() {
        // 10 visible chars total; clip to 5.
        let line = "\x1b[31mhello\x1b[32mworld\x1b[0m";
        let out = truncate_ansi_visible(line, 5);
        // Should contain "hello" plus the leading SGR; the second SGR
        // and "world" must NOT be in the output.
        assert!(out.contains("hello"));
        assert!(!out.contains("world"));
        assert!(out.contains("\x1b[31m"));
    }

    #[test]
    fn truncate_preserves_csi_state_changes_within_window() {
        // Multiple SGR resets inside the visible window. The walker emits
        // every escape it encounters along the way; we only stop at
        // visible chars beyond the budget.
        let line = "\x1b[31ma\x1b[32mb\x1b[33mc";
        let out = truncate_ansi_visible(line, 2);
        // Includes "a" and "b" plus their SGRs, excludes the visible 'c'.
        assert!(out.contains("\x1b[31m"));
        assert!(out.contains("\x1b[32m"));
        assert!(out.contains('a'));
        assert!(out.contains('b'));
        assert!(!out.contains('c'));
    }

    #[test]
    fn truncate_max_zero_returns_empty() {
        assert_eq!(truncate_ansi_visible("\x1b[31mxxx", 0), "");
    }

    #[test]
    fn truncate_handles_osc_with_st() {
        // OSC 0 (set title) terminated by ST (ESC \).
        let line = "\x1b]0;title\x1b\\hello world";
        let out = truncate_ansi_visible(line, 5);
        assert!(out.contains("\x1b]0;title\x1b\\"));
        assert!(out.contains("hello"));
        assert!(!out.contains("world"));
    }

    #[test]
    fn truncate_handles_osc_with_bel() {
        // OSC 0 terminated by BEL.
        let line = "\x1b]0;title\x07hello world";
        let out = truncate_ansi_visible(line, 5);
        assert!(out.contains("\x1b]0;title\x07"));
        assert!(out.contains("hello"));
        assert!(!out.contains("world"));
    }

    #[test]
    fn truncate_140_to_80_bounds_visible_count() {
        // A row similar to what a 140-col app emits when monitor is 80 cols.
        let body: String = (0..140).map(|_| 'X').collect();
        let line = format!("\x1b[31m{body}\x1b[0m");
        let out = truncate_ansi_visible(&line, 79);
        // Strip SGR sequences to count visible chars.
        let mut count = 0;
        let mut chars = out.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip CSI sequence
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        if (p as u32) >= 0x40 && (p as u32) <= 0x7E {
                            break;
                        }
                    }
                }
            } else {
                count += 1;
            }
        }
        assert_eq!(count, 79);
    }
}
