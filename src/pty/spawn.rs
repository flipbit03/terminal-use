use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd};

use anyhow::{Context, Result};
use nix::libc;
use nix::pty::{openpty, OpenptyResult};
use nix::sys::termios;
use nix::unistd::{close, execvp, fork, setsid, ForkResult, Pid};

use crate::daemon::protocol::TermSize;

/// Result of spawning a child process in a PTY.
pub struct PtyProcess {
    /// File descriptor for the PTY master (read output, write input).
    pub master_fd: OwnedFd,
    /// PID of the child process.
    pub pid: Pid,
}

/// Spawn a command in a new PTY with the given terminal size.
pub fn spawn(
    command: &str,
    args: &[String],
    size: &TermSize,
    env: &[(String, String)],
    cwd: Option<&str>,
    term: &str,
    shell: bool,
) -> Result<PtyProcess> {
    let win_size = nix::pty::Winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let OpenptyResult { master, slave } = openpty(&win_size, None).context("openpty failed")?;

    // Set reasonable terminal modes on the slave
    let mut termios = termios::tcgetattr(&slave).context("tcgetattr failed")?;
    termios.local_flags |= termios::LocalFlags::ECHO
        | termios::LocalFlags::ICANON
        | termios::LocalFlags::ISIG
        | termios::LocalFlags::IEXTEN;
    // Disable ECHOCTL (control-character caret-notation echoing). With
    // ECHOCTL on (macOS default for openpty), an input byte like 0x1B (ESC)
    // is echoed back as the two printable characters `^[`. That's mostly
    // harmless until an app like Midnight Commander writes its own ESC
    // sequences to a child shell PTY: mc's "persistent command buffer"
    // feature sends ESC `_` to trigger a zsh widget, then reads back the
    // echo expecting it to come through as raw bytes for its
    // `strip_ctrl_codes` filter. With ECHOCTL on, the echo is already
    // caret-notation printable text — strip_ctrl_codes can't strip it,
    // and the literal `^[_` gets baked into mc's prompt cache. Real
    // terminals end up with ECHOCTL off by the time interactive shells
    // run because the user's shell init (or zsh itself) disables it; we
    // start from openpty's defaults, so we have to do it ourselves.
    termios.local_flags &= !termios::LocalFlags::ECHOCTL;
    termios.input_flags |= termios::InputFlags::ICRNL;
    termios.output_flags |= termios::OutputFlags::OPOST | termios::OutputFlags::ONLCR;
    termios::tcsetattr(&slave, termios::SetArg::TCSANOW, &termios).context("tcsetattr failed")?;

    let slave_raw = slave.as_raw_fd();

    // Safety: we are about to fork. The child will exec immediately.
    match unsafe { fork() }.context("fork failed")? {
        ForkResult::Child => {
            // Close master in child
            let _ = close(master.as_raw_fd());

            // New session
            setsid().ok();

            // Set controlling terminal
            unsafe {
                libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0);
            }

            // Redirect stdio to slave PTY
            unsafe {
                libc::dup2(slave_raw, 0);
                libc::dup2(slave_raw, 1);
                libc::dup2(slave_raw, 2);
            }
            if slave_raw > 2 {
                let _ = close(slave_raw);
            }

            // Set environment. Do NOT set COLUMNS/LINES: they're only ever
            // correct at spawn-time and never update on resize. Libraries like
            // Python's shutil.get_terminal_size() read these env vars first and
            // fall back to TIOCGWINSZ only if unset, so leaving stale values
            // here causes those libraries (and anything that uses them, e.g.
            // Textual) to report the wrong size forever. TIOCGWINSZ is
            // authoritative and always current.
            std::env::set_var("TERM", term);
            std::env::remove_var("COLUMNS");
            std::env::remove_var("LINES");
            for (key, value) in env {
                std::env::set_var(key, value);
            }

            // Change directory
            if let Some(dir) = cwd {
                std::env::set_current_dir(dir).ok();
            }

            // Build the command
            let (exec_cmd, exec_args) = if shell {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
                let full_cmd = if args.is_empty() {
                    command.to_string()
                } else {
                    format!("{} {}", command, args.join(" "))
                };
                (shell, vec!["-c".to_string(), full_cmd])
            } else {
                (command.to_string(), args.to_vec())
            };

            let c_cmd = CString::new(exec_cmd.as_str()).unwrap();
            let mut c_args: Vec<CString> = vec![c_cmd.clone()];
            for a in &exec_args {
                c_args.push(CString::new(a.as_str()).unwrap());
            }

            // exec — does not return on success
            let _ = execvp(&c_cmd, &c_args);
            std::process::exit(127);
        }
        ForkResult::Parent { child } => {
            // Close slave in parent
            drop(slave);

            Ok(PtyProcess {
                master_fd: master,
                pid: child,
            })
        }
    }
}
