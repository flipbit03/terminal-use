mod commands;
mod daemon;
mod keys;
mod output;
mod pty;
mod render;

use clap::{Parser, Subcommand};

use daemon::protocol::TermSize;
use output::resolve_format;

#[derive(Debug, Parser)]
#[command(
    name = "tu",
    version,
    about = "Headless virtual terminal for AI agents"
)]
struct Cli {
    /// Output as JSON (auto-detected when stdout is not a TTY).
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print a compact LLM-friendly command reference.
    Usage,

    /// Spawn a process in a new virtual terminal.
    Run {
        /// Command to run.
        command: String,

        /// Arguments to the command.
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,

        /// Session name (default: "default").
        #[arg(long)]
        name: Option<String>,

        /// Terminal size as COLSxROWS (default: 120x40).
        #[arg(long, default_value = "120x40", value_parser = parse_size)]
        size: TermSize,

        /// Scrollback buffer lines (default: 1000).
        #[arg(long, default_value = "1000")]
        scrollback: usize,

        /// Extra environment variables (KEY=VAL).
        #[arg(long = "env", value_parser = parse_env)]
        envs: Vec<(String, String)>,

        /// Working directory.
        #[arg(long)]
        cwd: Option<String>,

        /// TERM environment variable (default: xterm-256color).
        #[arg(long, default_value = "xterm-256color")]
        term: String,

        /// Wrap command in $SHELL -c "...".
        #[arg(long)]
        shell: bool,
    },

    /// Kill process and remove session.
    Kill {
        /// Session name.
        #[arg(long, default_value = "default")]
        name: String,
    },

    /// List active sessions.
    List,

    /// Session info: pid, alive/exited, exit code, size.
    Status {
        /// Session name.
        #[arg(long, default_value = "default")]
        name: String,
    },

    /// Capture the terminal screen.
    Screenshot {
        /// Session name.
        #[arg(long, default_value = "default")]
        name: String,
        // TODO Phase 2: --png, --ansi, --html, --out
    },

    /// Print cursor position as row,col.
    Cursor {
        /// Session name.
        #[arg(long, default_value = "default")]
        name: String,
    },

    /// Print scrollback buffer.
    Scrollback {
        /// Session name.
        #[arg(long, default_value = "default")]
        name: String,

        /// Number of lines (default: all).
        #[arg(long)]
        lines: Option<usize>,
    },

    /// Type literal text into the terminal.
    Type {
        /// Text to type.
        text: String,

        /// Session name.
        #[arg(long, default_value = "default")]
        name: String,
    },

    /// Send keystrokes to the terminal.
    Press {
        /// Key names (space-separated): Enter, Tab, F1, Ctrl+C, Up, etc.
        #[arg(required = true)]
        keys: Vec<String>,

        /// Session name.
        #[arg(long, default_value = "default")]
        name: String,
    },

    /// Paste text using bracketed paste mode.
    Paste {
        /// Text to paste.
        text: String,

        /// Session name.
        #[arg(long, default_value = "default")]
        name: String,
    },

    /// Resize the terminal.
    Resize {
        /// New size as COLSxROWS (e.g. 160x50).
        #[arg(value_parser = parse_size)]
        size: TermSize,

        /// Session name.
        #[arg(long, default_value = "default")]
        name: String,
    },

    /// Wait for a condition on the terminal screen.
    Wait {
        /// Session name.
        #[arg(long, default_value = "default")]
        name: String,

        /// Wait until screen is unchanged for N milliseconds.
        #[arg(long)]
        stable: Option<u64>,

        /// Wait until regex matches screen content.
        #[arg(long)]
        text: Option<String>,

        /// Maximum wait time in milliseconds (default: 5000).
        #[arg(long, default_value = "5000")]
        timeout: u64,
    },

    /// Live read-only view of a session.
    Monitor {
        /// Session name (default: "default").
        #[arg(long, default_value = "default")]
        name: String,
    },

    /// Manage the background daemon.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Debug, Subcommand)]
enum DaemonAction {
    /// Start the daemon (foreground).
    Start,
    /// Stop the daemon.
    Stop,
    /// Show daemon status.
    Status,
}

fn parse_size(s: &str) -> Result<TermSize, String> {
    let parts: Vec<&str> = s.split('x').collect();
    if parts.len() != 2 {
        return Err(format!(
            "Invalid size format: {s:?}. Expected COLSxROWS (e.g. 120x40)"
        ));
    }
    let cols = parts[0]
        .parse::<u16>()
        .map_err(|_| format!("Invalid columns: {:?}", parts[0]))?;
    let rows = parts[1]
        .parse::<u16>()
        .map_err(|_| format!("Invalid rows: {:?}", parts[1]))?;
    Ok(TermSize { cols, rows })
}

fn parse_env(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("Invalid env format: {s:?}. Expected KEY=VALUE"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let format = resolve_format(cli.json);

    let result = match cli.command {
        // These commands don't need the daemon
        Command::Usage => {
            commands::usage::run().await;
            Ok(())
        }

        Command::Daemon { action } => match action {
            DaemonAction::Start => commands::daemon_cmd::start().await,
            DaemonAction::Stop => commands::daemon_cmd::stop().await,
            DaemonAction::Status => commands::daemon_cmd::status().await,
        },

        // All other commands talk to the daemon
        Command::Run {
            command,
            args,
            name,
            size,
            scrollback,
            envs,
            cwd,
            term,
            shell,
        } => {
            commands::run::run(
                command, args, name, size, scrollback, envs, cwd, term, shell, format,
            )
            .await
        }

        Command::Kill { name } => commands::kill::run(name).await,

        Command::List => commands::list::run(format).await,

        Command::Status { name } => commands::status::run(name, format).await,

        Command::Screenshot { name } => commands::screenshot::run(name, format).await,

        Command::Cursor { name } => commands::cursor::run(name, format).await,

        Command::Scrollback { name, lines } => commands::scrollback::run(name, lines, format).await,

        Command::Type { text, name } => commands::type_text::run(name, text).await,

        Command::Press { keys, name } => commands::press::run(name, keys).await,

        Command::Paste { text, name } => commands::paste::run(name, text).await,

        Command::Resize { size, name } => commands::resize::run(name, size).await,

        Command::Wait {
            name,
            stable,
            text,
            timeout,
        } => commands::wait::run(name, stable, text, timeout).await,

        Command::Monitor { name } => commands::monitor::run(name).await,
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
