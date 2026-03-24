use std::io::{self, Read, Write};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::daemon::protocol::{Request, Response};
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

async fn run_loop(tty: &mut RawTerminal, current_idx: &mut usize) -> Result<()> {
    let mut last_rows: Option<Vec<String>> = None;
    let mut last_term_size = get_terminal_size();

    loop {
        // Detect terminal resize → clear screen + force redraw
        let term_size = get_terminal_size();
        if term_size != last_term_size {
            // Full clear on resize to remove stale content
            print!("\x1b[2J");
            last_term_size = term_size;
            last_rows = None; // force redraw
        }

        let sessions = get_session_names().await.unwrap_or_default();
        if sessions.is_empty() {
            draw_waiting_screen()?;
            match tty.read_key(Duration::from_millis(250))? {
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
            }) => {
                if last_rows.as_ref() != Some(&rows_ansi) {
                    draw_frame(&sessions, *current_idx, &rows_ansi, rows, cols, term_size)?;
                    last_rows = Some(rows_ansi);
                }
            }
            Ok(Response::Error { message: _ }) => {
                last_rows = None;
            }
            _ => {}
        }

        match tty.read_key(Duration::from_millis(50))? {
            Some(Key::Quit) => break,
            Some(Key::Left) => {
                if *current_idx > 0 {
                    *current_idx -= 1;
                    last_rows = None;
                }
            }
            Some(Key::Right) => {
                let sessions = get_session_names().await.unwrap_or_default();
                if *current_idx + 1 < sessions.len() {
                    *current_idx += 1;
                    last_rows = None;
                }
            }
            None => {}
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

fn draw_frame(
    sessions: &[String],
    active_idx: usize,
    rows_ansi: &[String],
    sess_rows: u16,
    sess_cols: u16,
    term_size: (u16, u16),
) -> Result<()> {
    let (_term_cols, term_rows) = term_size;
    let mut out = io::stdout().lock();

    let frame_width = sess_cols as usize + 2;

    // Set terminal window title via OSC
    write!(out, "\x1b]0;tu monitor: {}\x07", &sessions[active_idx])?;

    // Move cursor home (no clear — we overwrite everything)
    write!(out, "\x1b[H")?;

    let mut row = 1u16; // 1-based terminal rows

    // Tab bar (only if multiple sessions)
    if sessions.len() > 1 && row <= term_rows {
        let tab_bar = build_tab_bar(sessions, active_idx);
        write!(out, "\x1b[{row};1H\x1b[2K{tab_bar}")?;
        row += 1;
    }

    // Top border: ┌─ name [COLSxROWS] ─...─┐
    if row <= term_rows {
        let title = format!(" {} [{}x{}] ", &sessions[active_idx], sess_cols, sess_rows);
        let border_len = frame_width.saturating_sub(3).saturating_sub(title.len());
        write!(
            out,
            "\x1b[{row};1H\x1b[2K\x1b[90m┌─\x1b[0m\x1b[1m{title}\x1b[0m\x1b[90m{border}┐\x1b[0m",
            border = "─".repeat(border_len),
        )?;
        row += 1;
    }

    // Content rows: │ <row content> │
    for r in 0..sess_rows as usize {
        if row > term_rows {
            break;
        }
        let line = rows_ansi.get(r).map(|s| s.as_str()).unwrap_or("");
        write!(
            out,
            "\x1b[{row};1H\x1b[2K\x1b[90m│\x1b[0m{line}\x1b[0m\x1b[{col}G\x1b[90m│\x1b[0m",
            col = frame_width,
        )?;
        row += 1;
    }

    // Bottom border: └─...─┘
    if row <= term_rows {
        write!(
            out,
            "\x1b[{row};1H\x1b[2K\x1b[90m└{border}┘\x1b[0m",
            border = "─".repeat(frame_width.saturating_sub(2)),
        )?;
    }

    // Status line — always pinned to the last row of the real terminal
    let status = if sessions.len() > 1 {
        "terminal-use monitor · ← → switch · Ctrl+C detach"
    } else {
        "terminal-use monitor · Ctrl+C detach"
    };
    write!(out, "\x1b[{term_rows};1H\x1b[2K\x1b[90m{status}\x1b[0m")?;

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

        // Enter alternate screen + hide cursor
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
        // Restore terminal: show cursor + leave alternate screen
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
