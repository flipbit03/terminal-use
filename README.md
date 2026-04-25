# terminal-use (`tu`)

Headless virtual terminal for AI agents. Spawn interactive terminal apps, read the screen, drive keyboard *and* mouse. No GUI, no X server, no display needed.

`tu` is to terminal applications what [agent-browser](https://github.com/vercel-labs/agent-browser) is to web pages.

## Demo

An AI agent playing NetHack — character creation, dungeon exploration, combat — driven entirely through `tu`:

https://github.com/user-attachments/assets/8dd87972-2ef5-4104-9074-52b6ee528e08

## What works

A real terminal emulator: anything that runs in your real terminal runs in `tu`.

- **Curses / TUI apps**: vim, less, htop, mc, top, nano, lazygit, tig, ranger.
- **Modern shell integration**: OSC 133 semantic prompts, OSC 7 working-directory hints, OSC 8 hyperlinks, APC, focus-event reporting — all consumed silently instead of leaking into the output as `^[…` artifacts.
- **Terminal queries**: DA / DCS terminfo / DECRQSS replies are answered automatically, so curses apps don't hang on startup waiting for terminal capability responses.
- **Mouse**: synthetic mouse input (click, drag, move, scroll), text-based targeting (`--on-text`, `--on-regex`), modifier keys, multi-click. Cursor glides between positions for real-mouse motion semantics.
- **Live monitor**: 30 fps read-only view with diff-based emission — fluid over SSH.

## Install

Prebuilt binary (Linux, macOS):

```bash
curl -fsSL https://raw.githubusercontent.com/flipbit03/terminal-use/main/install.sh | sh
```

From source:

```bash
cargo install terminal-use
```

To update:

```bash
tu self update
```

## Add to your agent

```
# terminal-use (`tu`)

Some programs (htop, vim, mc, dialog-based installers, ncurses UIs) need a
real terminal to render their interface — you can't just pipe stdin/stdout.
Use `tu` to run them in a virtual terminal, screenshot the screen, send
keystrokes, and drive the mouse. Run `tu usage` before the first
interaction for the full command reference.
```

That's it!

## Quick taste

```bash
# Spawn an app
tu run htop

# Read what's on screen (text or PNG)
tu screenshot
tu screenshot --png -o shot.png

# Send keystrokes
tu press F2                     # F2
tu press Escape : w q Enter     # save + quit vim
tu type "hello world"

# Drive the mouse — by coords, or by what's on screen
tu mouse click 50 20
tu mouse click --on-text "OK"
tu mouse click --on-text "Buy" --clicks 2          # double-click a label
tu mouse drag 10 10 50 30                          # drag from → to
tu mouse scroll down --amount 5

# Inspect mouse state (mode, encoding, synthetic cursor, held buttons)
tu mouse state

# Wait for screen state
tu wait --text "Complete" --timeout 10000
```

## `tu monitor`

Open a separate terminal and watch what your agent is doing in real time:

```bash
tu monitor                        # Watch the default session
tu monitor --name nethack         # Watch a specific session
```

- Full-color terminal rendering inside a framed window
- 30 fps refresh, diff-based emit — minimal bandwidth on SSH
- Shows the synthetic mouse cursor as a magenta `△` (filled when a button is held)
- Left/Right arrows to switch between sessions
- Handles terminal resize
- Ctrl+C to detach

## How it works

`tu` wraps a headless PTY + [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal) emulator behind a CLI. A background daemon manages sessions — each CLI invocation is stateless.

```
tu CLI --> Unix socket (JSON) --> daemon --> PTY + alacritty_terminal
                                                      ↑
                                          replies (DA, DCS, …)
                                          fed back to the inner app
```

- The emulator is alacritty's. It handles the full xterm command set including modern shell-integration sequences. Unknown / unsupported escapes are consumed cleanly rather than leaking into cell content.
- Replies the terminal owes the inner app (Device Attributes, cursor reports, DCS terminfo queries) are forwarded to the PTY via a writeback path — so vim, less, mc and friends boot without hanging.
- The daemon auto-starts on first use and auto-exits after 8 hours of inactivity.

## Defaults

- **Terminal size**: 120x40
- **TERM**: `xterm-256color`
- **Session name**: `default` (unless `--name` specified)
- **Output**: Human-readable if TTY, JSON for the agent (non-interactive terminal)

## License

MIT
