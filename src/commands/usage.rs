pub async fn run() {
    print!(
        r#"tu ("terminal-use") -- headless virtual terminal for AI agents

Spawn terminal apps, read the screen, send keystrokes. No GUI needed.
Default terminal size: 120x40 (TERM=xterm-256color).
All commands target the "default" session unless --name is given.

COMMANDS:
  run <cmd> [args...]             Spawn a process in a new virtual terminal
    --name <s>                      Session name (default: "default")
    --size <CxR>                    Terminal size (default: 120x40)
    --scrollback <n>                Scrollback lines (default: 1000)
    --env KEY=VAL                   Extra env vars (repeatable)
    --cwd <path>                    Working directory
    --term <TERM>                   TERM value (default: xterm-256color)
    --shell                         Wrap in $SHELL -c "..."
  kill [--name <s>]               Kill process and remove session
  list                            List active sessions
  status [--name <s>]             Session info: pid, alive/exited, exit code, size

  screenshot [--name <s>]         Capture the terminal screen as text
    --png                           Render as a PNG image instead of text
    --output <file>                 Output file path (default: auto temp file)
    --stdout                        Write PNG bytes to stdout (with --png)
    --font <path>                   Optional TTF font file (bundled: JetBrains Mono)
    --font-size <px>                Font size in pixels (default: 14, with --png)
  cursor [--name <s>]             Print cursor position as row,col
  scrollback [--name <s>]         Print scrollback buffer
    --lines <n>                     How many lines (default: all)

  type <text> [--name <s>]        Type literal text
  press <key>... [--name <s>]     Send keystrokes (space-separated)
  paste <text> [--name <s>]       Bracketed paste

  mouse click <col> <row>         Click at column,row (0-based, like cursor)
    --button left|right|middle      Default: left
    --mods Ctrl,Shift,Alt           Modifier combo (comma-separated)
    --clicks N                      Multi-click (2 = double, 3 = triple)
    --on-text <TEXT>                Click center of first text match instead
    --on-regex <RE>                 Click center of first regex match
    --match-index N                 Disambiguate when multiple matches (0-based)
    --force                         Send even if app has not enabled mouse mode
  mouse down|up <col> <row>       Press / release one half of a click
  mouse move <col> <row>          Move cursor (needs ButtonMotion / AnyMotion)
  mouse drag <c1> <r1> <c2> <r2>  Atomic down → motion path → up
  mouse scroll up|down|left|right [<col> <row>] [--amount N]
  mouse state [--name <s>]        Print mouse mode + encoding (or "disabled")

MOUSE CURSOR DISPLAY:
  When tu has a synthetic mouse cursor it shows up as a magenta △ glyph
  (filled magenta cell when a button is held) — visible in `tu monitor`
  and in `tu screenshot --png`. Text screenshots keep the body verbatim
  and append `△ tu mouse cursor at (col,row)` as a trailer below the
  rendered grid (so regex / grep over the body is never corrupted).
  `tu mouse state` is the canonical machine-readable source.

MOUSE TARGETING:
  Coords are 0-based and bounded by the current size; out-of-bounds errors out.
  --on-text / --on-regex search the visible screen left-to-right, top-to-bottom
  and click the center cell of the chosen match.
  Combine with --clicks for one-shot multi-click on a label:
    tu mouse click --on-text "Buy upgrade" --clicks 2
  Run `tu mouse state` first to confirm the inner app has DECSET 1000/1002/1006.
  If mode=None the click errors out — pass --force to send raw bytes anyway.

  resize <CxR> [--name <s>]      Resize terminal (e.g. 160x50)
  wait [--name <s>]               Wait for a condition
    --stable <ms>                   Screen unchanged for N ms
    --text <regex>                  Regex matches screen content
    --timeout <ms>                  Max wait (default: 5000)

  monitor [--name <s>]            Live read-only view of a session (← → to switch)
  daemon start|stop|status        Manage background daemon
  self update [--check]          Update tu to the latest version

KEYS:
  Letters/symbols    a, Z, !, @            Modifiers       Ctrl+C, Alt+F, Shift+Tab
  Navigation         Up Down Left Right    Ctrl combos     Ctrl+Z, Ctrl+L, Ctrl+D
                     Home End PageUp       Function        F1-F12
                     PageDown
  Editing            Enter Tab Space       Multi-key       press Down Down Down Enter
                     Escape Backspace
                     Delete Insert

OUTPUT:
  Default output is human-readable. Add --json for machine-readable JSON.
  When stdout is not a TTY, --json is auto-selected.

EXAMPLES:
  tu run htop                          Start htop
  tu screenshot                        Read the screen as text
  tu screenshot --png                  PNG to temp file, prints path
  tu screenshot --png -o shot.png      PNG to explicit path
  tu screenshot --png --stdout > s.png PNG bytes to stdout
  tu press F2                          Open htop setup
  tu press Escape : w q Enter          Save and quit vim
  tu type "hello world"                Type text into the terminal
  tu wait --text "Complete" --timeout 10000
  tu mouse state                       → mode=ButtonMotion encoding=Sgr
  tu mouse click 50 20                 Left-click at (col=50, row=20)
  tu mouse click --on-text "OK"        Click the OK button by label
  tu mouse click --on-text "Buy" --clicks 2   Double-click on "Buy"
  tu mouse click 10 5 --mods Ctrl      Ctrl+Click at (10,5)
  tu mouse drag 10 10 50 20            Drag from (10,10) to (50,20)
  tu mouse scroll down --amount 5      Scroll wheel down 5 ticks
  tu monitor                           Watch the session live
  tu kill                              End session
"#
    );
}
