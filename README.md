# terminal-use (`tu`)

Headless virtual terminal for AI agents. Spawn terminal apps, read the screen, send keystrokes. No GUI, no X server, no display needed.

`tu` is to terminal applications what [agent-browser](https://github.com/vercel-labs/agent-browser) is to web pages.

## Install

Prebuilt binary (Linux, macOS):

```bash
curl -fsSL https://raw.githubusercontent.com/flipbit03/terminal-use/main/install.sh | sh
```

From source:

```bash
cargo install terminal-use
```

## Quick start

```bash
# Spawn a process in a virtual terminal
tu run htop

# Read the screen
tu screenshot

# Send keystrokes
tu press F2
tu press Down Down Down Enter
tu type "hello world"

# Watch it live (read-only viewer)
tu monitor

# Clean up
tu kill
```

## How it works

`tu` wraps a headless PTY + [vt100](https://crates.io/crates/vt100) terminal emulator behind a CLI. A background daemon manages sessions — each CLI invocation is stateless.

```
tu CLI ──→ Unix socket (JSON) ──→ daemon ──→ PTY + vt100 emulator
```

The daemon auto-starts on first use and auto-exits after 5 minutes of inactivity.

## Commands

```
tu run <cmd> [args...]         Spawn a process in a virtual terminal
tu kill [--name <s>]           Kill session
tu list                        List active sessions
tu status [--name <s>]         Session info

tu screenshot [--name <s>]     Plain text screen dump
tu cursor [--name <s>]         Cursor position (row,col)
tu scrollback [--name <s>]     Scrollback buffer

tu type <text> [--name <s>]    Type literal text
tu press <key>... [--name <s>] Send keystrokes
tu paste <text> [--name <s>]   Bracketed paste

tu resize <CxR> [--name <s>]   Resize terminal
tu wait [--name <s>]           Wait for screen condition

tu monitor [--name <s>]        Live read-only viewer
tu usage                       LLM-friendly command reference
```

Run `tu usage` for the full reference (designed to be <1000 tokens for LLM consumption).

## Defaults

- **Terminal size**: 120x40
- **TERM**: `xterm-256color`
- **Session name**: `default` (unless `--name` specified)
- **Output**: Human-readable if TTY, JSON if piped

## Keys

```bash
tu press Enter                    # Single key
tu press Down Down Down Enter     # Multiple keys in sequence
tu press Ctrl+C                   # Modifier combos
tu press Alt+F                    # Alt prefix
tu press F5                       # Function keys
tu press Shift+Tab                # Shift combos
```

Full list: `Up Down Left Right Home End PageUp PageDown Backspace Delete Insert Tab Enter Space Escape F1-F12` plus `Ctrl+`, `Alt+`, `Shift+` modifiers.

## Named sessions

```bash
tu run htop --name monitoring
tu run vim --name editor
tu screenshot --name monitoring
tu press :wq Enter --name editor
tu monitor                        # ← → to switch between sessions
```

## `tu monitor`

Live read-only viewer for humans to watch what an agent is doing:

```bash
tu monitor                        # Watch the default session
tu monitor --name nethack         # Watch a specific session
```

- Full-color terminal rendering inside a framed window
- Left/Right arrows to switch between sessions
- Handles terminal resize
- Ctrl+C to detach

## For AI agents

Add to your agent's tools/skills:

```
When you need to interact with a terminal application, use `tu`.
Run `tu usage` to see the full command reference.
```

The agent runs `tu usage`, gets a <1000 token cheatsheet, and is fully equipped.

## License

MIT
