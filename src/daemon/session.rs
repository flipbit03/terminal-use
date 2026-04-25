use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;

use anyhow::{Context, Result};
use nix::libc;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;

use crate::daemon::protocol::{CursorPos, MouseButton, MouseLastEvent, SessionInfo, TermSize};
use crate::pty;

/// Tu's idea of the synthetic mouse state for a session: where the cursor was
/// left after the most recent emitted event, which buttons are still held
/// (down without a matching up), and a snapshot of the most recent event.
///
/// The inner application is under no obligation to render the mouse cursor,
/// so this is the only authoritative source for "where am I and what's held"
/// when an agent loses track between calls.
#[derive(Debug, Default)]
pub struct MouseTracker {
    pub cursor: Option<CursorPos>,
    pub buttons_held: Vec<MouseButton>,
    pub last_event: Option<MouseLastEvent>,
}

impl MouseTracker {
    pub fn record_position(&mut self, col: u16, row: u16) {
        self.cursor = Some(CursorPos { row, col });
    }

    pub fn press(&mut self, button: MouseButton) {
        if !self.buttons_held.contains(&button) {
            self.buttons_held.push(button);
        }
    }

    pub fn release(&mut self, button: MouseButton) {
        self.buttons_held.retain(|b| *b != button);
    }

    /// Clear the cursor if it is now outside the new size.
    pub fn clamp_to_size(&mut self, size: &TermSize) {
        if let Some(pos) = self.cursor {
            if pos.col >= size.cols || pos.row >= size.rows {
                self.cursor = None;
            }
        }
    }
}

/// A terminal session: a child process in a PTY with a vt100 screen buffer.
pub struct Session {
    pub name: String,
    pub master_fd: OwnedFd,
    pub pid: Pid,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub size: TermSize,
    pub alive: bool,
    pub exit_code: Option<i32>,
    pub mouse: MouseTracker,
}

impl Session {
    /// Spawn a new session.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: String,
        command: &str,
        args: &[String],
        size: TermSize,
        scrollback: usize,
        env: &[(String, String)],
        cwd: Option<&str>,
        term: &str,
        shell: bool,
    ) -> Result<Self> {
        let pty_proc = pty::spawn::spawn(command, args, &size, env, cwd, term, shell)?;
        let parser = vt100::Parser::new(size.rows, size.cols, scrollback);

        Ok(Self {
            name,
            master_fd: pty_proc.master_fd,
            pid: pty_proc.pid,
            parser: Arc::new(Mutex::new(parser)),
            size,
            alive: true,
            exit_code: None,
            mouse: MouseTracker::default(),
        })
    }

    /// Start a background task that reads PTY output and feeds it to the vt100 parser.
    pub fn start_reader(&self) -> Result<()> {
        let parser = self.parser.clone();

        // Duplicate the fd so the async reader owns it independently
        let dup_fd = nix::unistd::dup(&self.master_fd).context("dup master_fd")?;

        tokio::spawn(async move {
            // Safety: we just dup'd the fd, so this is a valid owned fd.
            let std_file = unsafe { std::fs::File::from_raw_fd(dup_fd.as_raw_fd()) };
            // Prevent the OwnedFd from closing separately — std_file now owns the underlying fd
            std::mem::forget(dup_fd);

            let mut async_file = tokio::io::BufReader::new(tokio::fs::File::from_std(std_file));
            let mut buf = [0u8; 4096];

            loop {
                match async_file.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut p = parser.lock().await;
                        p.process(&buf[..n]);
                    }
                    Err(e) => {
                        if e.raw_os_error() == Some(libc::EIO) {
                            break;
                        }
                        eprintln!("PTY read error: {e}");
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    /// Check if the child is still alive, updating status if it exited.
    pub fn poll_status(&mut self) {
        if !self.alive {
            return;
        }
        match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {}
            Ok(WaitStatus::Exited(_, code)) => {
                self.alive = false;
                self.exit_code = Some(code);
            }
            Ok(WaitStatus::Signaled(_, sig, _)) => {
                self.alive = false;
                self.exit_code = Some(128 + sig as i32);
            }
            Ok(_) => {}
            Err(_) => {
                self.alive = false;
            }
        }
    }

    /// Get the current screen contents as plain text.
    pub async fn screenshot_text(&self) -> String {
        let parser = self.parser.lock().await;
        let screen = parser.screen();
        let mut lines = Vec::with_capacity(self.size.rows as usize);
        for row in 0..self.size.rows {
            let mut line = String::new();
            for col in 0..self.size.cols {
                let cell = screen.cell(row, col).unwrap();
                let ch = cell.contents();
                if ch.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&ch);
                }
            }
            let trimmed = line.trim_end();
            lines.push(trimmed.to_string());
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Get the current screen contents with ANSI formatting (raw bytes).
    pub async fn screenshot_ansi(&self) -> Vec<u8> {
        let parser = self.parser.lock().await;
        let screen = parser.screen();
        screen.contents_formatted()
    }

    /// Get the screen as a vector of ANSI-rendered row strings.
    /// Each string contains SGR escape codes for colors/attributes, suitable for
    /// embedding inside a frame (no cursor positioning escapes).
    pub async fn screenshot_cells(&self) -> Vec<String> {
        let parser = self.parser.lock().await;
        let screen = parser.screen();
        let mut rows = Vec::with_capacity(self.size.rows as usize);

        for row in 0..self.size.rows {
            let mut line = String::new();
            let mut prev_fg = vt100::Color::Default;
            let mut prev_bg = vt100::Color::Default;
            let mut prev_bold = false;
            let mut prev_inverse = false;
            let mut prev_underline = false;

            for col in 0..self.size.cols {
                let cell = screen.cell(row, col).unwrap();

                // Skip wide continuation cells
                if cell.is_wide_continuation() {
                    continue;
                }

                let fg = cell.fgcolor();
                let bg = cell.bgcolor();
                let bold = cell.bold();
                let inverse = cell.inverse();
                let underline = cell.underline();

                // Emit SGR changes
                let attrs_changed = fg != prev_fg
                    || bg != prev_bg
                    || bold != prev_bold
                    || inverse != prev_inverse
                    || underline != prev_underline;

                if attrs_changed {
                    // Reset and re-apply all active attributes
                    line.push_str("\x1b[0");
                    if bold {
                        line.push_str(";1");
                    }
                    if underline {
                        line.push_str(";4");
                    }
                    if inverse {
                        line.push_str(";7");
                    }
                    push_fg_sgr(&mut line, fg);
                    push_bg_sgr(&mut line, bg);
                    line.push('m');

                    prev_fg = fg;
                    prev_bg = bg;
                    prev_bold = bold;
                    prev_inverse = inverse;
                    prev_underline = underline;
                }

                let ch = cell.contents();
                if ch.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(&ch);
                }
            }

            // Reset at end of row
            line.push_str("\x1b[0m");
            rows.push(line);
        }

        rows
    }

    /// Get the current cursor position.
    pub async fn cursor_pos(&self) -> CursorPos {
        let parser = self.parser.lock().await;
        let screen = parser.screen();
        CursorPos {
            row: screen.cursor_position().0,
            col: screen.cursor_position().1,
        }
    }

    /// Get scrollback contents.
    pub async fn scrollback(&self, lines: Option<usize>) -> String {
        let parser = self.parser.lock().await;
        let screen = parser.screen();
        let full = screen.contents();
        match lines {
            Some(n) => {
                let all_lines: Vec<&str> = full.lines().collect();
                let start = all_lines.len().saturating_sub(n);
                all_lines[start..].join("\n")
            }
            None => full,
        }
    }

    /// Get session info.
    pub fn info(&mut self) -> SessionInfo {
        self.poll_status();
        SessionInfo {
            name: self.name.clone(),
            pid: self.pid.as_raw() as u32,
            alive: self.alive,
            exit_code: self.exit_code,
            size: self.size.clone(),
        }
    }

    /// Write raw bytes to the PTY (keystrokes).
    pub fn write_bytes(&self, data: &[u8]) -> Result<()> {
        pty::input::write_to_pty(&self.master_fd, data)
    }

    /// Type text (write as-is).
    pub fn type_text(&self, text: &str) -> Result<()> {
        pty::input::write_to_pty(&self.master_fd, text.as_bytes())
    }

    /// Paste text using bracketed paste mode.
    pub fn paste_text(&self, text: &str) -> Result<()> {
        pty::input::bracketed_paste(&self.master_fd, text)
    }

    /// Resize the terminal.
    pub async fn resize(&mut self, size: TermSize) -> Result<()> {
        pty::resize::resize_pty(&self.master_fd, &size)?;
        let mut parser = self.parser.lock().await;
        parser.set_size(size.rows, size.cols);
        self.size = size.clone();
        self.mouse.clamp_to_size(&size);
        Ok(())
    }

    /// Kill the child process.
    pub fn kill(&mut self) {
        if self.alive {
            let _ = nix::sys::signal::kill(self.pid, nix::sys::signal::Signal::SIGTERM);
            std::thread::sleep(std::time::Duration::from_millis(100));
            self.poll_status();
            if self.alive {
                let _ = nix::sys::signal::kill(self.pid, nix::sys::signal::Signal::SIGKILL);
                let _ = waitpid(self.pid, None);
                self.alive = false;
                self.exit_code = Some(137);
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.kill();
    }
}

fn push_fg_sgr(s: &mut String, color: vt100::Color) {
    match color {
        vt100::Color::Default => {}
        vt100::Color::Idx(i) => {
            if i < 8 {
                s.push_str(&format!(";{}", 30 + i));
            } else if i < 16 {
                s.push_str(&format!(";{}", 90 + i - 8));
            } else {
                s.push_str(&format!(";38;5;{}", i));
            }
        }
        vt100::Color::Rgb(r, g, b) => {
            s.push_str(&format!(";38;2;{};{};{}", r, g, b));
        }
    }
}

fn push_bg_sgr(s: &mut String, color: vt100::Color) {
    match color {
        vt100::Color::Default => {}
        vt100::Color::Idx(i) => {
            if i < 8 {
                s.push_str(&format!(";{}", 40 + i));
            } else if i < 16 {
                s.push_str(&format!(";{}", 100 + i - 8));
            } else {
                s.push_str(&format!(";48;5;{}", i));
            }
        }
        vt100::Color::Rgb(r, g, b) => {
            s.push_str(&format!(";48;2;{};{};{}", r, g, b));
        }
    }
}
