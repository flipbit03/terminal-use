# terminal-use (`tu`)

Headless virtual terminal for AI agents. Binary is called `tu`. See issue #1 for the full project spec.

## Build & Test

```bash
cargo build          # dev build
cargo build --release
cargo test           # run unit tests
cargo fmt --check    # check formatting (CI enforces this)
cargo clippy -- -D warnings  # lint (CI enforces this)
RUSTDOCFLAGS="-Dwarnings" cargo doc --all-features --no-deps  # doc lint (CI enforces this)
```

## Architecture

CLI client → Unix socket (JSON-over-newline) → background daemon → per-session PTY + vt100 emulator.

- `src/main.rs` — clap CLI, command dispatch
- `src/daemon/server.rs` — Unix socket listener, daemon lifecycle
- `src/daemon/session.rs` — PTY + vt100 per terminal session
- `src/daemon/manager.rs` — session map, request handling
- `src/daemon/protocol.rs` — JSON request/response types
- `src/pty/` — PTY spawn, input injection, resize
- `src/keys.rs` — key name → escape sequence mapping
- `src/commands/` — CLI command handlers (each talks to daemon)
- `src/render/` — text snapshot and image screenshot renderers

## Conventions

- Follow lineark patterns: `usage` command for LLM reference, `version = "0.0.0"` patched by CI, `binary-release` feature flag.
- `usage` ≠ `--help`. `usage` is a hand-maintained <1000 token cheatsheet for agents. `--help` is verbose clap output for humans.
- Commands that don't need the daemon (usage, self update) are handled before daemon connection.
- Default terminal size: 120x40, TERM=xterm-256color.

## Git Commits

- Do NOT include `Co-Authored-By` attribution in commits
- Do NOT include the "Generated with Claude Code" footer in commits
