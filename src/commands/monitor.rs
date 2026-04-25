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
    // Diff state: the previously-emitted frame as a vector of one ANSI string per
    // visible terminal row. Each new frame is rendered into a fresh `Vec<String>`
    // and compared row-by-row; only changed rows are written to stdout. This is
    // the bulk of the flicker fix — most rows stay byte-identical between frames.
    let mut prev_frame: Option<Vec<String>> = None;
    // True the first time we observe a session after the waiting state. We
    // give the inner app a brief moment to finish its initial paint before
    // snapshotting — without this, a freshly-spawned mc/vim/htop is often
    // mid-render on the first vt100 read and the user sees a partial frame.
    let mut just_attached = true;

    let fetch_interval = Duration::from_millis(FRAME_INTERVAL_MS);

    loop {
        // Detect terminal resize → clear screen + force redraw
        let term_size = get_terminal_size();
        if term_size != last_term_size {
            print!("\x1b[2J");
            last_term_size = term_size;
            last_rows = None;
            prev_frame = None;
            last_fetch = std::time::Instant::now() - fetch_interval; // force refetch
        }

        // Fetch + redraw at ~30fps.
        if last_fetch.elapsed() >= fetch_interval {
            last_fetch = std::time::Instant::now();

            let sessions = get_session_names().await.unwrap_or_default();
            if sessions.is_empty() {
                draw_waiting_screen()?;
                prev_frame = None;
                just_attached = true;
                match tty.read_key(Duration::from_millis(FRAME_INTERVAL_MS))? {
                    Some(Key::Quit) => break,
                    _ => continue,
                }
            }
            if *current_idx >= sessions.len() {
                *current_idx = sessions.len() - 1;
            }

            // Just emerged from the waiting state — let the inner app finish
            // its initial paint before snapshotting.
            if just_attached {
                tokio::time::sleep(Duration::from_millis(150)).await;
                just_attached = false;
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
                    emit_frame_diff(prev_frame.as_deref(), &new_frame, &sessions[*current_idx])?;
                    prev_frame = Some(new_frame);
                }
                Ok(Response::Error { message: _ }) => {
                    last_rows = None;
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
                prev_frame = None;
                last_fetch = std::time::Instant::now() - fetch_interval; // force refetch
            }
            Some(Key::Right) => {
                let sessions = get_session_names().await.unwrap_or_default();
                if *current_idx + 1 < sessions.len() {
                    *current_idx += 1;
                    last_rows = None;
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
    let mid_row = rows / 2 - 2;
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
            format!("\x1b[90m{left_border}\x1b[0m{line}\x1b[0m")
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

/// SGR-wrapped glyph for tu's synthetic mouse cursor. Idle = magenta `△`;
/// held = bright-white `△` on a magenta cell.
fn mouse_cursor_glyph(held: bool) -> &'static str {
    if held {
        "\x1b[1;48;5;201;97m△"
    } else {
        "\x1b[1;38;5;201m△"
    }
}

/// Diff the new frame against the previously-emitted one and only re-emit rows
/// that actually changed.
///
/// (We avoid DECSET 2026 / synchronized output mode here: in practice it
/// behaves inconsistently across the terminals we care about — at least one
/// SSH terminal we tested ate frames mid-sync. The diff alone gives most of
/// the flicker reduction; the per-row CSI 2K + write is small enough that
/// terminals with their own paint coalescing handle it cleanly.)
fn emit_frame_diff(prev: Option<&[String]>, new: &[String], session_name: &str) -> Result<()> {
    let mut out = io::stdout().lock();

    // First emission after a transition (startup, session switch, error,
    // resize) — wipe the alt screen so we know we're rendering on top of a
    // clean slate. Without this, leftovers from the waiting screen, a previous
    // session's frame, or partial mid-render artifacts can survive on rows
    // the diff later considers "unchanged" and never re-emits.
    if prev.is_none() {
        write!(out, "\x1b[2J\x1b[H")?;
        write!(out, "\x1b]0;tu monitor: {session_name}\x07")?;
    }

    let prev_len = prev.map(|p| p.len()).unwrap_or(0);
    for (i, line) in new.iter().enumerate() {
        let unchanged = prev
            .and_then(|p| p.get(i))
            .map(|s| s == line)
            .unwrap_or(false);
        if unchanged {
            continue;
        }
        // 1-indexed terminal row.
        let row = i + 1;
        write!(out, "\x1b[{row};1H\x1b[2K{line}")?;
    }

    // If the new frame is shorter than the previous one, clear what's beyond.
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

        // Enter alternate screen + hide cursor + disable autowrap.
        //
        // Autowrap-off (DECRST 7) matters with the diff-based emit path: if a
        // session is wider than the user's terminal, raw cell content would
        // overflow into the row below, and the diff would never re-clear that
        // row when its model contents stayed "the same". With autowrap off
        // the overflow is silently dropped and rows stay confined to their
        // own line.
        print!("\x1b[?1049h\x1b[?25l\x1b[?7l");
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
        // Restore terminal: re-enable autowrap, show cursor, leave alt screen.
        print!("\x1b[?7h\x1b[?25h\x1b[?1049l");
        let _ = io::stdout().flush();

        // Restore original termios
        let _ = nix::sys::termios::tcsetattr(
            io::stdin(),
            nix::sys::termios::SetArg::TCSANOW,
            &self.original_termios,
        );
    }
}
