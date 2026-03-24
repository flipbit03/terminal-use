use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;

use anyhow::{Context, Result};
use nix::libc;

use crate::daemon::protocol::TermSize;

/// Resize the PTY. This sends SIGWINCH to the child process.
pub fn resize_pty(master: &OwnedFd, size: &TermSize) -> Result<()> {
    let win_size = nix::pty::Winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let ret = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &win_size) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error()).context("TIOCSWINSZ ioctl failed");
    }
    Ok(())
}
