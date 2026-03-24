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

  screenshot [--name <s>]         Plain text screen dump
  cursor [--name <s>]             Print cursor position as row,col
  scrollback [--name <s>]         Print scrollback buffer
    --lines <n>                     How many lines (default: all)

  type <text> [--name <s>]        Type literal text
  press <key>... [--name <s>]     Send keystrokes (space-separated)
  paste <text> [--name <s>]       Bracketed paste

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
  tu screenshot                        Read the screen
  tu press F2                          Open htop setup
  tu press Escape : w q Enter          Save and quit vim
  tu type "hello world"                Type text into the terminal
  tu wait --text "Complete" --timeout 10000
  tu monitor                           Watch the session live
  tu kill                              End session
"#
    );
}
