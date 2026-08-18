//! Regression test for issue #28: sessions created right after `tu daemon stop`
//! used to land on the dying daemon (100ms grace window before `exit(0)`) and
//! silently vanish with it, leaving stale socket/pid files behind.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn tu(runtime_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tu"))
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .args(args)
        .output()
        .expect("failed to run tu")
}

/// Stops the daemon and removes the runtime dir even when an assertion
/// panics mid-test — otherwise a failed run leaks a daemon (and its `cat`
/// sessions) that the 8h idle timeout never reaps.
struct DaemonGuard(PathBuf);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = tu(&self.0, &["daemon", "stop"]);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn sessions_created_right_after_daemon_stop_survive() {
    // Keep the path short: Unix socket paths are capped at ~108 chars.
    let dir = PathBuf::from(format!("/tmp/tu-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = DaemonGuard(dir.clone());

    for i in 0..3 {
        assert!(
            tu(&dir, &["run", "--name", "warm", "--", "cat"])
                .status
                .success(),
            "failed to start warm session"
        );
        assert!(
            tu(&dir, &["daemon", "stop"]).status.success(),
            "daemon stop failed"
        );

        // `stop` must not return before the daemon is fully gone.
        assert!(!dir.join("tu.sock").exists(), "stale tu.sock after stop");
        assert!(!dir.join("tu.pid").exists(), "stale tu.pid after stop");

        // A session created immediately after `stop` returns must land on a
        // fresh daemon and still exist once the old grace window would have
        // expired.
        let name = format!("s{i}");
        assert!(
            tu(&dir, &["run", "--name", &name, "--", "cat"])
                .status
                .success(),
            "failed to create session {name}"
        );
        std::thread::sleep(Duration::from_millis(300));
        let status = tu(&dir, &["status", "--name", &name]);
        assert!(
            status.status.success(),
            "session {name} vanished: {}{}",
            String::from_utf8_lossy(&status.stdout),
            String::from_utf8_lossy(&status.stderr)
        );
        tu(&dir, &["kill", "--name", &name]);
    }
}
