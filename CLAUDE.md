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

CLI client → Unix socket (JSON-over-newline) → background daemon → per-session PTY + alacritty_terminal emulator.

- `src/main.rs` — clap CLI, command dispatch
- `src/daemon/server.rs` — Unix socket listener, daemon lifecycle. `Wait` and `Mouse` requests run outside the manager lock so they don't block `monitor` polls.
- `src/daemon/session.rs` — PTY + emulator per session, plus a per-session `MouseTracker` (synthetic cursor, held buttons, last event)
- `src/daemon/manager.rs` — session map, request handling, `handle_mouse_glided` (interpolated mouse motion)
- `src/daemon/protocol.rs` — JSON request/response types, mouse action/target/state types
- `src/emu.rs` — wrapper around `alacritty_terminal` + its embedded `vte::ansi` parser; exposes the small Parser/Screen/Cell/Color slice the rest of the codebase consumes. Capture proxy queues `Event::PtyWrite` replies (DA, DCS, etc.) for the reader to forward back to the PTY.
- `src/pty/` — PTY spawn (ECHOCTL off so curses subshell apps see clean ESC echo), input injection, resize
- `src/keys.rs` — key name → escape sequence mapping
- `src/mouse.rs` — wire encoders for SGR / Default / UTF-8 mouse protocols, screen-search helpers (`find_text` / `find_regex`)
- `src/commands/` — CLI command handlers (each talks to daemon)
- `src/render/` — text and image screenshot renderers (PNG paints a magenta `△` cursor overlay)

## Conventions

- Follow lineark patterns: `usage` command for LLM reference, `version = "0.0.0"` patched by CI, `binary-release` feature flag.
- `usage` ≠ `--help`. `usage` is a hand-maintained <1000 token cheatsheet for agents. `--help` is verbose clap output for humans.
- Commands that don't need the daemon (usage, self update) are handled before daemon connection.
- Default terminal size: 120x40, TERM=xterm-256color.

## Git Commits

- Do NOT include `Co-Authored-By` attribution in commits
- Do NOT include the "Generated with Claude Code" footer in commits
