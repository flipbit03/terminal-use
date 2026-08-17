use std::io::{self, Read, Write};
use std::time::Duration;

use anyhow::{Context, Result};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
    // Clickable tab regions from the most recent frame, used to resolve a
    // mouse click on the tab bar into a session switch.
    let mut tab_hits: Vec<TabHit> = Vec::new();

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
                        &new_frame.lines,
                        &sessions[*current_idx],
                    )?;
                    needs_clear = false;
                    tab_hits = new_frame.tab_hits;
                    prev_frame = Some(new_frame.lines);
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
            Some(Key::Click { row, col }) => {
                // Resolve the clicked tab to a session name, then look that
                // name up in a freshly-fetched list so a concurrent reorder
                // can't switch to the wrong window.
                if let Some(name) = tab_hits
                    .iter()
                    .find(|h| h.term_row == row && col >= h.start_col && col <= h.end_col)
                    .map(|h| h.name.clone())
                {
                    let sessions = get_session_names().await.unwrap_or_default();
                    if let Some(idx) = sessions.iter().position(|s| s == &name) {
                        if idx != *current_idx {
                            *current_idx = idx;
                            last_rows = None;
                            needs_clear = true;
                            prev_frame = None;
                            last_fetch = std::time::Instant::now() - fetch_interval;
                        }
                    }
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

/// A rendered monitor frame: one ANSI string per visible terminal row
/// (index 0 = terminal row 1), plus the clickable tab regions for mouse
/// hit-testing.
struct Frame {
    lines: Vec<String>,
    tab_hits: Vec<TabHit>,
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
) -> Frame {
    let (term_cols, term_rows) = term_size;
    let multi = sessions.len() > 1;

    // Lay out the tab bar first: with many windows it wraps onto multiple
    // lines, and the header height (and thus content area) depends on how
    // many lines that takes. The tab bar starts on terminal row 2, right
    // below the status bar.
    let (tab_lines, tab_hits) = if multi {
        layout_tabs(sessions, active_idx, term_cols, 2)
    } else {
        (Vec::new(), Vec::new())
    };

    let frame_width = sess_cols as usize + 2;
    let tcols = term_cols as usize;
    let cropped_right = tcols < frame_width;

    // Layout: status(1) + tab bar(tab_lines) + top border(1) + content + bottom border(1)
    let header_rows = 1 + tab_lines.len() as u16 + 1;
    let available_content_rows = term_rows.saturating_sub(header_rows + 1);
    let content_rows_to_show = sess_rows.min(available_content_rows);
    let cropped_bottom = content_rows_to_show < sess_rows;

    let mut frame: Vec<String> = Vec::with_capacity(term_rows as usize);

    // Row 1: status bar. Clip the plain text to the terminal width *before*
    // coloring: an un-clipped status longer than the terminal autowraps onto
    // the tab bar row, and since the status text changes every second while
    // the (unchanged) tab bar is skipped by the frame diff, that overflow
    // would clobber the first tab line and never get repaired.
    let elapsed = format_elapsed(since_last_change);
    let status = if multi {
        format!("terminal-use monitor · last change {elapsed} · ← → or click · Ctrl+C detach")
    } else {
        format!("terminal-use monitor · last change {elapsed} · Ctrl+C detach")
    };
    // Clip by display width (the status contains ambiguous-width glyphs like
    // ← → ·), not scalar count, so it can't autowrap on wide-glyph terminals.
    let status = truncate_ansi_visible(&status, tcols);
    frame.push(format!("\x1b[90m{status}\x1b[0m"));

    // Tab bar (one or more wrapped lines). layout_tabs wraps to fit, but a
    // lone tab whose name exceeds the terminal width can still overflow, so
    // clip defensively for the same no-autowrap reason as the status line.
    for line in tab_lines {
        let clipped = truncate_ansi_visible(&line, tcols);
        frame.push(format!("{clipped}\x1b[0m"));
    }

    // Top border
    {
        let title = format!(" {} [{}x{}] ", sessions[active_idx], sess_cols, sess_rows);
        // Display columns, not bytes: session names are arbitrary UTF-8, and a
        // byte count would under-fill the dashes for é/CJK/emoji names.
        let prefix_width = 2 + title.width();
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

    Frame {
        lines: frame,
        tab_hits,
    }
}

/// Truncate an ANSI-decorated line to at most `max_visible` printable
/// characters, copying SGR / OSC / other escape sequences verbatim. Used
/// when the user's terminal is narrower than the session frame: without
/// this, the daemon's `sess_cols` visible chars on each row would exceed
/// the user's terminal width and autowrap onto the next row, scrambling
/// neighbouring rows on every frame.
///
/// Visible-width counting uses `unicode-width` (display columns), so CJK
/// wide chars count as 2 and zero-width / combining marks as 0. A char is
/// only emitted if it fits entirely within the remaining column budget, so
/// the result never exceeds `max_visible` columns (a wide char straddling
/// the boundary is dropped rather than half-printed).
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
            let w = UnicodeWidthChar::width(c).unwrap_or(0);
            if visible + w > max_visible {
                break;
            }
            out.push(c);
            visible += w;
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

/// A clickable tab region in the (possibly wrapped) tab bar.
#[derive(Clone, Debug, PartialEq)]
struct TabHit {
    /// 1-indexed terminal row the tab label occupies.
    term_row: u16,
    /// 1-indexed inclusive terminal column range covered by the label
    /// (including its one space of padding on each side).
    start_col: u16,
    end_col: u16,
    /// Session name this tab selects. Stored by name (not list index) so a
    /// click stays correct even if the session list reordered between the
    /// frame being built and the click landing.
    name: String,
}

/// Lay out the session tabs into one or more wrapped lines and return the
/// rendered ANSI strings plus the click regions.
///
/// Tabs are packed left-to-right separated by ` │ `; when the next tab
/// (plus its separator) would overflow `term_cols`, layout wraps to a new
/// line. This is what lets a large window count stay visible — instead of
/// running off the right edge, the bar grows downward like tmux's window
/// list. `base_row` is the 1-indexed terminal row of the first tab line so
/// the returned hits carry absolute coordinates for click hit-testing.
fn layout_tabs(
    sessions: &[String],
    active_idx: usize,
    term_cols: u16,
    base_row: u16,
) -> (Vec<String>, Vec<TabHit>) {
    let max = (term_cols.max(1)) as usize;
    let mut lines: Vec<String> = Vec::new();
    let mut hits: Vec<TabHit> = Vec::new();
    let mut cur = String::new();
    let mut col = 0usize; // visible columns used so far on the current line

    for (i, name) in sessions.iter().enumerate() {
        let label_w = UnicodeWidthStr::width(name.as_str()) + 2; // " name "
        let sep_w = if col > 0 { 3 } else { 0 }; // " │ "
                                                 // Wrap when the tab won't fit and the current line already has
                                                 // something on it (a lone over-wide tab is allowed to overflow).
        if col > 0 && col + sep_w + label_w > max {
            lines.push(std::mem::take(&mut cur));
            col = 0;
        }
        if col > 0 {
            cur.push_str("\x1b[90m │ \x1b[0m");
            col += 3;
        }
        let start_col = col + 1; // 1-indexed
        if i == active_idx {
            // Active: bold + inverse
            cur.push_str(&format!("\x1b[1;7m {} \x1b[0m", name));
        } else {
            // Inactive: dim
            cur.push_str(&format!("\x1b[2m {} \x1b[0m", name));
        }
        col += label_w;
        hits.push(TabHit {
            term_row: base_row + lines.len() as u16,
            start_col: start_col as u16,
            end_col: col as u16, // 1-indexed inclusive
            name: name.clone(),
        });
    }
    lines.push(cur);
    (lines, hits)
}

// --- Raw terminal handling ---

enum Key {
    Quit,
    Left,
    Right,
    /// Left-button press at a 1-indexed terminal (row, col).
    Click {
        row: u16,
        col: u16,
    },
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

        // Enter alternate screen + hide cursor, and enable mouse reporting
        // (?1000h = button press/release tracking, ?1006h = SGR extended
        // coordinates so columns/rows past 223 are reported). This lets the
        // user click a tab to switch sessions, like tmux mouse mode.
        print!("\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h");
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

        // Large enough to hold a full SGR mouse report (ESC [ < b ; x ; y M),
        // which can be ~16 bytes for three-digit coordinates.
        let mut buf = [0u8; 32];
        let n = io::stdin().lock().read(&mut buf).unwrap_or(0);
        if n == 0 {
            return Ok(None);
        }

        // Ctrl+C = 0x03, q = 0x71
        if buf[0] == 0x03 || buf[0] == b'q' {
            return Ok(Some(Key::Quit));
        }

        // SGR mouse report: ESC [ < b ; x ; y (M = press | m = release).
        if n >= 4 && buf[0] == 0x1b && buf[1] == b'[' && buf[2] == b'<' {
            return Ok(parse_sgr_mouse(&buf[3..n]));
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

/// Parse the body of an SGR mouse report — the bytes after `ESC [ <`, up to
/// and including the terminating `M`/`m`. Returns a left-button *press* as a
/// `Key::Click`; releases, non-left buttons, motion and wheel events are
/// ignored (return `None`).
///
/// Only the first event in `body` is parsed: a fast click can deliver press
/// and release coalesced into one read, so we stop at the first terminator
/// rather than treating the whole buffer as a single (malformed) sequence.
fn parse_sgr_mouse(body: &[u8]) -> Option<Key> {
    let term_pos = body.iter().position(|&c| c == b'M' || c == b'm')?;
    let is_press = body[term_pos] == b'M';
    let nums = std::str::from_utf8(&body[..term_pos]).ok()?;
    let mut parts = nums.split(';');
    let button: u32 = parts.next()?.parse().ok()?;
    let col: u16 = parts.next()?.parse().ok()?;
    let row: u16 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None; // more than three fields → not a well-formed report
    }
    // SGR packs the button number into the low two bits, OR-ed with modifier
    // flags (Shift 4, Alt 8, Ctrl 16), motion (32) and extra/wheel button
    // bits (64, 128). Mask off only the modifier bits so a *modified* left
    // click (e.g. Ctrl+click) still counts, while still requiring button 0
    // and rejecting motion/wheel. Act on press only so a click switches once.
    const MODIFIERS: u32 = 4 | 8 | 16;
    if is_press && (button & !MODIFIERS) == 0 {
        Some(Key::Click { row, col })
    } else {
        None
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        // Restore terminal: disable mouse reporting, show cursor, leave alt
        // screen.
        print!("\x1b[?1000l\x1b[?1006l\x1b[?25h\x1b[?1049l");
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
    use super::{build_frame_strings, layout_tabs, parse_sgr_mouse, truncate_ansi_visible, Key};

    fn names(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("win{i}")).collect()
    }

    /// Count visible (printable) columns in an ANSI-decorated line, skipping
    /// CSI escape sequences. Adequate for the box-drawing / ASCII content the
    /// monitor emits.
    fn visible_width(line: &str) -> usize {
        let mut count = 0;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        if (0x40..=0x7E).contains(&(p as u32)) {
                            break;
                        }
                    }
                }
            } else {
                count += 1;
            }
        }
        count
    }

    #[test]
    fn header_lines_never_exceed_terminal_width() {
        // 15 windows in a 60-col terminal: status + wrapped tab bar. None of
        // the rendered rows may exceed the width, or they autowrap and corrupt
        // neighbouring rows under the frame diff.
        let sessions = names(15);
        let rows_ansi: Vec<String> = (0..40).map(|_| String::new()).collect();
        let frame = build_frame_strings(
            &sessions,
            0,
            &rows_ansi,
            40,
            120,
            (60, 44),
            std::time::Duration::from_secs(5),
            None,
            false,
        );
        for line in &frame.lines {
            assert!(
                visible_width(line) <= 60,
                "row exceeds terminal width: {line:?}"
            );
        }
    }

    #[test]
    fn layout_single_line_when_it_fits() {
        let sessions = names(3);
        let (lines, hits) = layout_tabs(&sessions, 0, 200, 2);
        assert_eq!(lines.len(), 1);
        assert_eq!(hits.len(), 3);
        // All hits on the same (first) terminal row.
        assert!(hits.iter().all(|h| h.term_row == 2));
        // First tab " win1 " occupies columns 1..=6.
        assert_eq!((hits[0].start_col, hits[0].end_col), (1, 6));
        // Second tab follows the 3-col " │ " separator: 6 + 3 + 1 = 10.
        assert_eq!(hits[1].start_col, 10);
    }

    #[test]
    fn layout_wraps_when_too_many_tabs() {
        let sessions = names(12);
        // Narrow terminal forces wrapping onto multiple rows.
        let (lines, hits) = layout_tabs(&sessions, 0, 30, 2);
        assert!(
            lines.len() > 1,
            "expected wrapping, got {} line(s)",
            lines.len()
        );
        // Every tab is represented and rows increase with wrapping.
        assert_eq!(hits.len(), 12);
        assert!(hits.iter().any(|h| h.term_row > 2));
        // Hits never start past the terminal width.
        assert!(hits.iter().all(|h| h.start_col >= 1 && h.start_col <= 30));
        // Rows are non-decreasing in tab order.
        for w in hits.windows(2) {
            assert!(w[1].term_row >= w[0].term_row);
        }
    }

    #[test]
    fn layout_hit_columns_match_label_width() {
        let sessions = names(1);
        let (_lines, hits) = layout_tabs(&sessions, 0, 80, 2);
        // " win1 " = 6 visible columns, 1-indexed inclusive.
        assert_eq!((hits[0].start_col, hits[0].end_col), (1, 6));
    }

    #[test]
    fn parse_left_press_returns_click() {
        // ESC [ < 0 ; 12 ; 3 M  →  left press at col 12 row 3.
        match parse_sgr_mouse(b"0;12;3M") {
            Some(Key::Click { row, col }) => {
                assert_eq!((row, col), (3, 12));
            }
            other => panic!("expected Click, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn parse_release_is_ignored() {
        assert!(parse_sgr_mouse(b"0;12;3m").is_none());
    }

    #[test]
    fn parse_non_left_button_is_ignored() {
        // Right button (2) press.
        assert!(parse_sgr_mouse(b"2;12;3M").is_none());
        // Wheel-up (64).
        assert!(parse_sgr_mouse(b"64;12;3M").is_none());
        // Left button with motion bit (32) — a drag, not a click.
        assert!(parse_sgr_mouse(b"32;12;3M").is_none());
    }

    #[test]
    fn parse_modified_left_click_returns_click() {
        // Modified left presses still switch tabs: Shift (4), Alt (8),
        // Ctrl (16), and a combination (4|8|16 = 28).
        for b in [4u32, 8, 16, 28] {
            match parse_sgr_mouse(format!("{b};9;2M").as_bytes()) {
                Some(Key::Click { row, col }) => assert_eq!((row, col), (2, 9)),
                _ => panic!("expected Click for modified left press (button {b})"),
            }
        }
    }

    #[test]
    fn truncate_counts_wide_chars_by_display_width() {
        // Three CJK wide chars = 6 display columns. Clipping to 5 keeps only
        // the first two (4 cols); the third would straddle the boundary.
        let line = "日本語";
        let out = truncate_ansi_visible(line, 5);
        assert_eq!(out, "日本");
        // Exact fit keeps all three.
        assert_eq!(truncate_ansi_visible(line, 6), "日本語");
    }

    #[test]
    fn layout_wraps_on_display_width_for_wide_names() {
        // Two names of 6 display columns each (" name " = 8 cols) plus the
        // 3-col separator can't share a 12-col line, so they wrap.
        let sessions = vec!["日本語".to_string(), "한국어".to_string()];
        let (lines, hits) = layout_tabs(&sessions, 0, 12, 2);
        assert_eq!(lines.len(), 2, "wide names should wrap by display width");
        assert_eq!(hits[0].term_row, 2);
        assert_eq!(hits[1].term_row, 3);
        // " 日本語 " = 8 display columns → cols 1..=8.
        assert_eq!((hits[0].start_col, hits[0].end_col), (1, 8));
    }

    #[test]
    fn parse_coalesced_press_release_takes_first() {
        // A fast click can deliver press+release in one read; only the
        // leading press should be acted on.
        match parse_sgr_mouse(b"0;5;7M\x1b[<0;5;7m") {
            Some(Key::Click { row, col }) => assert_eq!((row, col), (7, 5)),
            _ => panic!("expected Click from leading press"),
        }
    }

    #[test]
    fn parse_malformed_is_ignored() {
        assert!(parse_sgr_mouse(b"garbage").is_none());
        assert!(parse_sgr_mouse(b"0;12M").is_none()); // too few fields
    }

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
