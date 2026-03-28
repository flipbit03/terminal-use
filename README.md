# terminal-use (`tu`)

Headless virtual terminal for AI agents. Spawn interactive terminal apps, read the screen, send keystrokes. No GUI, no X server, no display needed.

`tu` is to terminal applications what [agent-browser](https://github.com/vercel-labs/agent-browser) is to web pages.

## Demo

An AI agent playing NetHack — character creation, dungeon exploration, combat — driven entirely through `tu`:

https://github.com/user-attachments/assets/8dd87972-2ef5-4104-9074-52b6ee528e08

## Install

Prebuilt binary (Linux, macOS):

```bash
curl -fsSL https://raw.githubusercontent.com/flipbit03/terminal-use/main/install.sh | sh
```

From source:

```bash
cargo install terminal-use
```

Linux source builds that include image screenshots need development packages
for `fontconfig` and `freetype` available to `pkg-config`. On Debian/Ubuntu:

```bash
sudo apt-get install -y pkg-config libfontconfig1-dev libfreetype6-dev
```

To update:

```bash
tu self update
```

## Add to your agent

```
# terminal-use (`tu`)

Some programs (htop, vim, mc, dialog-based installers, ncurses UIs) need a real
terminal to render their interface — you can't just pipe stdin/stdout. Use `tu`
to run them in a virtual terminal, snapshot the screen as text, capture a real
screenshot when needed, and send keystrokes.
Run `tu usage` before the first interaction for the full command reference.
```

That's it!

## `tu monitor`

Open a separate terminal and watch what your agent is doing in real time:

```bash
tu monitor                        # Watch the default session
tu monitor --name nethack         # Watch a specific session
```

- Full-color terminal rendering inside a framed window
- Left/Right arrows to switch between sessions
- Handles terminal resize
- Ctrl+C to detach

## Capture Output

Read the current screen as text:

```bash
tu snapshot
tu snapshot --json
```

Capture the current rendering as an image:

```bash
tu screenshot current.png
tu screenshot current.jpg
tu screenshot --stdout > current.png
```

## How it works

`tu` wraps a headless PTY + [vt100](https://crates.io/crates/vt100) terminal emulator behind a CLI. A background daemon manages sessions — each CLI invocation is stateless.

```
tu CLI --> Unix socket (JSON) --> daemon --> PTY + vt100 emulator
```

The daemon auto-starts on first use and auto-exits after 8 hours of inactivity.

## Defaults

- **Terminal size**: 120x40
- **TERM**: `xterm-256color`
- **Session name**: `default` (unless `--name` specified)
- **Output**: Human-readable if TTY, JSON for the agent (non-interactive terminal)

## License

MIT
