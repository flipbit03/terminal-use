use std::ffi::{CString, OsStr, OsString};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use nix::fcntl::{fcntl, FcntlArg, FdFlag};
use nix::libc;
use nix::pty::{openpty, OpenptyResult};
use nix::sys::termios;
use nix::unistd::{chdir, close, execve, fork, setsid, ForkResult, Pid};

use crate::daemon::protocol::TermSize;

/// Result of spawning a child process in a PTY.
pub struct PtyProcess {
    /// File descriptor for the PTY master (read output, write input).
    pub master_fd: OwnedFd,
    /// PID of the child process.
    pub pid: Pid,
}

/// Insert or replace `key` in the child environment being assembled.
fn env_upsert(env: &mut Vec<(OsString, OsString)>, key: &str, value: &OsStr) {
    let key = OsStr::new(key);
    if let Some(pair) = env.iter_mut().find(|(k, _)| k.as_os_str() == key) {
        pair.1 = value.to_os_string();
    } else {
        env.push((key.to_os_string(), value.to_os_string()));
    }
}

/// Resolve `command` the way execvp would, but *before* forking: a name
/// containing a slash is used as-is, otherwise the first executable match on
/// `path_var` wins. Returns the name unchanged when nothing matches so the
/// later execve fails with an ordinary ENOENT.
fn resolve_command(command: &str, path_var: Option<&OsStr>) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    if command.contains('/') {
        return PathBuf::from(command);
    }
    if let Some(paths) = path_var {
        for dir in std::env::split_paths(paths) {
            let candidate = dir.join(command);
            if let Ok(meta) = candidate.metadata() {
                if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                    return candidate;
                }
            }
        }
    }
    PathBuf::from(command)
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

    // Close-on-exec on both PTY fds: this process forks concurrently (the
    // daemon spawns sessions from multiple tokio workers, and tests fork in
    // parallel), and a child forked for an unrelated spawn must not inherit
    // this PTY and keep it open past its owner. The child's own stdio copies
    // survive exec because dup2 clears FD_CLOEXEC on the duplicate.
    fcntl(&master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))
        .context("set FD_CLOEXEC on PTY master")?;
    fcntl(&slave, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).context("set FD_CLOEXEC on PTY slave")?;

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

    // Everything the child needs after fork() is prepared up front. This
    // process is multi-threaded, so between fork and exec the child may only
    // make async-signal-safe calls: another thread could hold the env lock or
    // a malloc arena lock at fork time, and any allocation or getenv/setenv
    // in the forked child would deadlock it before it ever execs. Hence no
    // set_var, no CString::new, and no PATH search after the fork — the child
    // just dup2s, chdirs and execves.

    // Build the child environment: inherit ours, minus COLUMNS/LINES. Those
    // are only ever correct at spawn-time and never update on resize.
    // Libraries like Python's shutil.get_terminal_size() read these env vars
    // first and fall back to TIOCGWINSZ only if unset, so leaving stale
    // values here causes those libraries (and anything that uses them, e.g.
    // Textual) to report the wrong size forever. TIOCGWINSZ is authoritative
    // and always current.
    let mut child_env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            key.as_os_str() != OsStr::new("COLUMNS") && key.as_os_str() != OsStr::new("LINES")
        })
        .collect();
    env_upsert(&mut child_env, "TERM", OsStr::new(term));
    // Advertise 24-bit color: tu renders truecolor faithfully, so tell child
    // apps that gate emission on $COLORTERM to emit it. Applied before the
    // caller's --env overrides so `--env COLORTERM=...` can still override it.
    env_upsert(&mut child_env, "COLORTERM", OsStr::new("truecolor"));
    for (key, value) in env {
        env_upsert(&mut child_env, key, OsStr::new(value));
    }
    let env_lookup = |key: &str| {
        child_env
            .iter()
            .find(|(k, _)| k.as_os_str() == OsStr::new(key))
            .map(|(_, v)| v.as_os_str())
    };

    // Build the command (against the child env, so --env SHELL/PATH apply)
    let (exec_cmd, exec_args) = if shell {
        let shell_prog = env_lookup("SHELL")
            .and_then(|v| v.to_str())
            .unwrap_or("/bin/sh")
            .to_string();
        let full_cmd = if args.is_empty() {
            command.to_string()
        } else {
            format!("{} {}", command, args.join(" "))
        };
        (shell_prog, vec!["-c".to_string(), full_cmd])
    } else {
        (command.to_string(), args.to_vec())
    };

    let exec_path = resolve_command(&exec_cmd, env_lookup("PATH"));
    let c_path = CString::new(exec_path.as_os_str().as_bytes().to_vec())
        .context("NUL byte in command path")?;
    let c_cmd = CString::new(exec_cmd.as_str()).context("NUL byte in command")?;
    let mut c_args: Vec<CString> = vec![c_cmd];
    for a in &exec_args {
        c_args.push(CString::new(a.as_str()).context("NUL byte in argument")?);
    }
    let mut c_env: Vec<CString> = Vec::with_capacity(child_env.len());
    for (key, value) in &child_env {
        let mut kv = key.as_bytes().to_vec();
        kv.push(b'=');
        kv.extend_from_slice(value.as_bytes());
        c_env.push(CString::new(kv).context("NUL byte in child environment")?);
    }
    let c_cwd = cwd
        .map(CString::new)
        .transpose()
        .context("NUL byte in cwd")?;

    // Safety: we are about to fork. The child only makes async-signal-safe
    // calls (dup2/chdir/execve on pre-built buffers) and execs immediately.
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

            // Change directory
            if let Some(dir) = &c_cwd {
                let _ = chdir(dir.as_c_str());
            }

            // exec — does not return on success
            let _ = execve(&c_path, &c_args, &c_env);
            // _exit, not exit: atexit handlers aren't fork-safe.
            unsafe { libc::_exit(127) }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;
    use std::time::{Duration, Instant};

    use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
    use nix::sys::signal::{kill, Signal};
    use nix::sys::wait::waitpid;

    /// Spawn `sh -c` printing `$COLORTERM` followed by a sentinel, and read
    /// the PTY until the sentinel shows up. Reading to a sentinel instead of
    /// EOF keeps the test off platform-specific PTY EOF semantics (Linux
    /// reports EIO after the child exits, BSDs may discard late output), and
    /// the deadline makes a wedged child fail this test instead of hanging
    /// the whole `cargo test` run.
    fn colorterm_seen_by_child(env: &[(String, String)]) -> String {
        let size = TermSize { cols: 80, rows: 24 };
        // "sh" without a slash also exercises the pre-fork PATH resolution.
        let proc = spawn(
            "sh",
            &[
                "-c".to_string(),
                r#"printf '<%s>EOT' "$COLORTERM""#.to_string(),
            ],
            &size,
            env,
            None,
            "xterm-256color",
            false,
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut out = Vec::new();
        let mut buf = [0u8; 1024];
        while !out.windows(3).any(|w| w == b"EOT") {
            if Instant::now() >= deadline {
                let _ = kill(proc.pid, Signal::SIGKILL);
                let _ = waitpid(proc.pid, None);
                panic!(
                    "timed out waiting for child output; got {:?}",
                    String::from_utf8_lossy(&out)
                );
            }
            let mut fds = [PollFd::new(proc.master_fd.as_fd(), PollFlags::POLLIN)];
            match poll(&mut fds, PollTimeout::from(200u16)) {
                Ok(0) => continue, // poll timeout — re-check the deadline
                Ok(_) => {}
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => panic!("poll on PTY master failed: {e}"),
            }
            match nix::unistd::read(&proc.master_fd, &mut buf) {
                Ok(0) => break, // EOF before sentinel — let the assert report
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(nix::errno::Errno::EINTR) => continue,
                Err(nix::errno::Errno::EIO) => break, // Linux: child gone
                Err(e) => panic!("read from PTY master failed: {e}"),
            }
        }
        let _ = waitpid(proc.pid, None);
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn advertises_colorterm_truecolor_by_default() {
        // The child environment is assembled from this process's env, and dev
        // shells usually already export COLORTERM=truecolor — inherited as-is,
        // the assert would pass even with the advertisement deleted. So pin
        // the parent side: once with COLORTERM absent, once with a
        // conflicting inherited value that must be replaced.
        std::env::remove_var("COLORTERM");
        let seen = colorterm_seen_by_child(&[]);
        assert!(seen.contains("<truecolor>"), "child saw: {seen:?}");

        std::env::set_var("COLORTERM", "256color");
        let seen = colorterm_seen_by_child(&[]);
        std::env::remove_var("COLORTERM");
        assert!(seen.contains("<truecolor>"), "child saw: {seen:?}");
    }

    #[test]
    fn resolve_command_searches_path_and_keeps_slashed_names() {
        assert_eq!(
            resolve_command("./rel/prog", Some(OsStr::new("/bin"))),
            PathBuf::from("./rel/prog")
        );
        let sh = resolve_command("sh", Some(OsStr::new("/nonexistent:/usr/bin:/bin")));
        assert!(
            sh.as_path() == OsStr::new("/usr/bin/sh") || sh.as_path() == OsStr::new("/bin/sh"),
            "resolved to {sh:?}"
        );
        // Unresolvable names come back unchanged so execve gets a clean ENOENT.
        assert_eq!(
            resolve_command("no-such-binary-tu-test", Some(OsStr::new("/nonexistent"))),
            PathBuf::from("no-such-binary-tu-test")
        );
    }

    #[test]
    fn explicit_env_overrides_colorterm() {
        let env = vec![("COLORTERM".to_string(), "24bit".to_string())];
        let seen = colorterm_seen_by_child(&env);
        assert!(seen.contains("<24bit>"), "child saw: {seen:?}");
    }
}
