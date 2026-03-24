use std::os::fd::OwnedFd;

use anyhow::{Context, Result};
use nix::unistd::write;

/// Write raw bytes to the PTY master (equivalent to the user typing).
pub fn write_to_pty(master: &OwnedFd, data: &[u8]) -> Result<()> {
    let mut offset = 0;
    while offset < data.len() {
        let n = write(master, &data[offset..]).context("write to PTY failed")?;
        offset += n;
    }
    Ok(())
}

/// Send text as bracketed paste (signals to the app that this is pasted, not typed).
pub fn bracketed_paste(master: &OwnedFd, text: &str) -> Result<()> {
    write_to_pty(master, b"\x1b[200~")?;
    write_to_pty(master, text.as_bytes())?;
    write_to_pty(master, b"\x1b[201~")?;
    Ok(())
}
