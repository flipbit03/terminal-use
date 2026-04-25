//! `tu mouse …` CLI dispatch.
//!
//! Coordinates exposed to the user are 0-based, matching `cursor` /
//! `screenshot`. Targeting flags (`--on-text`, `--on-regex`) replace positional
//! coords by searching the visible screen and clicking the center of the
//! resolved match.

use anyhow::{bail, Result};
use clap::{Args, Subcommand};

use crate::daemon::protocol::{
    MouseAction, MouseButton, MouseTarget, Request, Response, ScrollDir,
};
use crate::daemon::server::{ensure_daemon, send_request};
use crate::mouse;
use crate::output::Format;

#[derive(Debug, Subcommand)]
pub enum MouseCmd {
    /// Click at coordinates or on text.
    Click {
        /// Column (0-based). Omit when using --on-text / --on-regex.
        col: Option<u16>,
        /// Row (0-based). Omit when using --on-text / --on-regex.
        row: Option<u16>,

        #[command(flatten)]
        target: TargetOpts,

        #[command(flatten)]
        common: CommonOpts,

        /// Mouse button.
        #[arg(long, default_value = "left", value_parser = mouse::parse_button)]
        button: MouseButton,

        /// Multi-click count (2 = double-click, 3 = triple-click).
        #[arg(long, default_value = "1")]
        clicks: u32,
    },

    /// Press a button without releasing.
    Down {
        col: Option<u16>,
        row: Option<u16>,

        #[command(flatten)]
        target: TargetOpts,

        #[command(flatten)]
        common: CommonOpts,

        #[arg(long, default_value = "left", value_parser = mouse::parse_button)]
        button: MouseButton,
    },

    /// Release a button.
    Up {
        col: Option<u16>,
        row: Option<u16>,

        #[command(flatten)]
        target: TargetOpts,

        #[command(flatten)]
        common: CommonOpts,

        #[arg(long, default_value = "left", value_parser = mouse::parse_button)]
        button: MouseButton,
    },

    /// Move the cursor (requires AnyMotion or ButtonMotion mode).
    Move {
        col: Option<u16>,
        row: Option<u16>,

        #[command(flatten)]
        target: TargetOpts,

        #[command(flatten)]
        common: CommonOpts,
    },

    /// Drag from one cell to another (atomic down → motion → up).
    Drag {
        col1: u16,
        row1: u16,
        col2: u16,
        row2: u16,

        #[command(flatten)]
        common: CommonOpts,

        #[arg(long, default_value = "left", value_parser = mouse::parse_button)]
        button: MouseButton,
    },

    /// Scroll wheel.
    Scroll {
        /// Direction.
        #[arg(value_parser = mouse::parse_scroll_dir)]
        dir: ScrollDir,

        col: Option<u16>,
        row: Option<u16>,

        #[command(flatten)]
        target: TargetOpts,

        #[command(flatten)]
        common: CommonOpts,

        /// Number of wheel notches.
        #[arg(long, default_value = "1")]
        amount: u32,
    },

    /// Print the inner app's mouse mode + encoding.
    State {
        #[arg(long, default_value = "default")]
        name: String,
    },
}

#[derive(Debug, Args, Clone, Default)]
pub struct TargetOpts {
    /// Click on first match of literal text on the visible screen.
    #[arg(long, value_name = "TEXT")]
    pub on_text: Option<String>,
    /// Click on first match of regex.
    #[arg(long, value_name = "REGEX")]
    pub on_regex: Option<String>,
    /// 0-based index when there are multiple matches.
    #[arg(long, default_value = "0")]
    pub match_index: usize,
}

#[derive(Debug, Args, Clone)]
pub struct CommonOpts {
    /// Modifier keys (comma-separated): Ctrl,Shift,Alt.
    #[arg(long, default_value = "")]
    pub mods: String,
    /// Session name.
    #[arg(long, default_value = "default")]
    pub name: String,
    /// Send even if the inner app has not enabled mouse reporting.
    #[arg(long)]
    pub force: bool,
}

pub async fn run(cmd: MouseCmd, format: Format) -> Result<()> {
    ensure_daemon()?;

    match cmd {
        MouseCmd::State { name } => run_state(name, format).await,
        MouseCmd::Click {
            col,
            row,
            target,
            common,
            button,
            clicks,
        } => {
            let target = build_target(col, row, &target)?;
            let mods = mouse::parse_mods(&common.mods)?;
            send_action(
                &common.name,
                MouseAction::Click {
                    target,
                    button,
                    mods,
                    clicks,
                },
                common.force,
            )
            .await
        }
        MouseCmd::Down {
            col,
            row,
            target,
            common,
            button,
        } => {
            let target = build_target(col, row, &target)?;
            let mods = mouse::parse_mods(&common.mods)?;
            send_action(
                &common.name,
                MouseAction::Down {
                    target,
                    button,
                    mods,
                },
                common.force,
            )
            .await
        }
        MouseCmd::Up {
            col,
            row,
            target,
            common,
            button,
        } => {
            let target = build_target(col, row, &target)?;
            let mods = mouse::parse_mods(&common.mods)?;
            send_action(
                &common.name,
                MouseAction::Up {
                    target,
                    button,
                    mods,
                },
                common.force,
            )
            .await
        }
        MouseCmd::Move {
            col,
            row,
            target,
            common,
        } => {
            let target = build_target(col, row, &target)?;
            let mods = mouse::parse_mods(&common.mods)?;
            send_action(
                &common.name,
                MouseAction::Move { target, mods },
                common.force,
            )
            .await
        }
        MouseCmd::Drag {
            col1,
            row1,
            col2,
            row2,
            common,
            button,
        } => {
            let mods = mouse::parse_mods(&common.mods)?;
            send_action(
                &common.name,
                MouseAction::Drag {
                    from: MouseTarget::Coords {
                        col: col1,
                        row: row1,
                    },
                    to: MouseTarget::Coords {
                        col: col2,
                        row: row2,
                    },
                    button,
                    mods,
                },
                common.force,
            )
            .await
        }
        MouseCmd::Scroll {
            dir,
            col,
            row,
            target,
            common,
            amount,
        } => {
            let mods = mouse::parse_mods(&common.mods)?;
            // For scroll, target is optional (defaults to top-left).
            let resolved = build_optional_target(col, row, &target)?;
            send_action(
                &common.name,
                MouseAction::Scroll {
                    target: resolved,
                    dir,
                    amount,
                    mods,
                },
                common.force,
            )
            .await
        }
    }
}

async fn run_state(name: String, format: Format) -> Result<()> {
    match send_request(&Request::MouseState { name }).await? {
        Response::MouseState {
            mode,
            encoding,
            size,
            cursor,
            buttons_held,
            last_event,
        } => {
            use crate::daemon::protocol::MouseMode;
            match format {
                Format::Human => {
                    if mode == MouseMode::None {
                        println!("disabled");
                    } else {
                        println!("mode={mode:?} encoding={encoding:?}");
                    }
                    let cursor_str = match cursor {
                        Some(p) => format!("({},{})", p.col, p.row),
                        None => "none".into(),
                    };
                    println!("cursor={cursor_str} screensize={}x{}", size.cols, size.rows);
                    let held_str = if buttons_held.is_empty() {
                        "none".to_string()
                    } else {
                        buttons_held
                            .iter()
                            .map(|b| format!("{b:?}").to_lowercase())
                            .collect::<Vec<_>>()
                            .join(",")
                    };
                    println!("buttons_held={held_str}");
                    match last_event {
                        Some(ev) => {
                            let kind = format!("{:?}", ev.kind);
                            let extra = match (ev.button, ev.scroll_dir) {
                                (Some(b), _) => format!(" button={b:?}").to_lowercase(),
                                (_, Some(d)) => format!(" dir={d:?}").to_lowercase(),
                                _ => String::new(),
                            };
                            let mods_str = mods_to_str(&ev.mods);
                            let mods_part = if mods_str.is_empty() {
                                String::new()
                            } else {
                                format!(" mods={mods_str}")
                            };
                            println!(
                                "last_event={} ({},{}){}{} at {}",
                                kind, ev.col, ev.row, extra, mods_part, ev.ts_unix
                            );
                        }
                        None => println!("last_event=none"),
                    }
                }
                Format::Json => {
                    let v = serde_json::json!({
                        "mode": format!("{mode:?}"),
                        "encoding": format!("{encoding:?}"),
                        "enabled": mode != MouseMode::None,
                        "size": { "cols": size.cols, "rows": size.rows },
                        "cursor": cursor.map(|p| serde_json::json!({"col": p.col, "row": p.row})),
                        "buttons_held": buttons_held
                            .iter()
                            .map(|b| format!("{b:?}").to_lowercase())
                            .collect::<Vec<_>>(),
                        "last_event": last_event,
                    });
                    println!("{v}");
                }
            }
            Ok(())
        }
        Response::Error { message } => bail!("{message}"),
        other => bail!("Unexpected response: {other:?}"),
    }
}

fn mods_to_str(mods: &crate::daemon::protocol::MouseMods) -> String {
    let mut parts = Vec::new();
    if mods.ctrl {
        parts.push("ctrl");
    }
    if mods.shift {
        parts.push("shift");
    }
    if mods.alt {
        parts.push("alt");
    }
    parts.join(",")
}

async fn send_action(name: &str, action: MouseAction, force: bool) -> Result<()> {
    let req = Request::Mouse {
        name: name.to_string(),
        action,
        force,
    };
    match send_request(&req).await? {
        Response::Ok => Ok(()),
        Response::Error { message } => bail!("{message}"),
        other => bail!("Unexpected response: {other:?}"),
    }
}

fn build_target(col: Option<u16>, row: Option<u16>, t: &TargetOpts) -> Result<MouseTarget> {
    let positional = match (col, row) {
        (Some(c), Some(r)) => Some((c, r)),
        (None, None) => None,
        _ => bail!("provide both <col> and <row>, or neither (with --on-text / --on-regex)"),
    };

    match (positional, t.on_text.as_deref(), t.on_regex.as_deref()) {
        (Some((c, r)), None, None) => Ok(MouseTarget::Coords { col: c, row: r }),
        (None, Some(needle), None) => Ok(MouseTarget::Text {
            needle: needle.to_string(),
            match_index: t.match_index,
        }),
        (None, None, Some(pat)) => Ok(MouseTarget::Regex {
            pattern: pat.to_string(),
            match_index: t.match_index,
        }),
        (None, None, None) => bail!("no target: provide <col> <row>, --on-text, or --on-regex"),
        _ => bail!("specify exactly one of: positional <col> <row>, --on-text, --on-regex"),
    }
}

fn build_optional_target(
    col: Option<u16>,
    row: Option<u16>,
    t: &TargetOpts,
) -> Result<Option<MouseTarget>> {
    if col.is_none() && row.is_none() && t.on_text.is_none() && t.on_regex.is_none() {
        return Ok(None);
    }
    Ok(Some(build_target(col, row, t)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_target() -> TargetOpts {
        TargetOpts::default()
    }

    #[test]
    fn build_target_coords_pair() {
        let t = build_target(Some(5), Some(7), &empty_target()).unwrap();
        assert!(matches!(t, MouseTarget::Coords { col: 5, row: 7 }));
    }

    #[test]
    fn build_target_on_text() {
        let mut opts = empty_target();
        opts.on_text = Some("Buy".into());
        let t = build_target(None, None, &opts).unwrap();
        assert!(matches!(t, MouseTarget::Text { ref needle, match_index: 0 } if needle == "Buy"));
    }

    #[test]
    fn build_target_on_regex_with_match_index() {
        let mut opts = empty_target();
        opts.on_regex = Some(r"\d+".into());
        opts.match_index = 2;
        let t = build_target(None, None, &opts).unwrap();
        assert!(
            matches!(t, MouseTarget::Regex { ref pattern, match_index: 2 } if pattern == r"\d+")
        );
    }

    #[test]
    fn build_target_partial_coords_errors() {
        assert!(build_target(Some(1), None, &empty_target()).is_err());
        assert!(build_target(None, Some(1), &empty_target()).is_err());
    }

    #[test]
    fn build_target_no_input_errors() {
        assert!(build_target(None, None, &empty_target()).is_err());
    }

    #[test]
    fn build_target_conflicting_inputs_errors() {
        let mut opts = empty_target();
        opts.on_text = Some("X".into());
        assert!(build_target(Some(1), Some(1), &opts).is_err());

        let mut opts = empty_target();
        opts.on_text = Some("X".into());
        opts.on_regex = Some("Y".into());
        assert!(build_target(None, None, &opts).is_err());
    }

    #[test]
    fn build_optional_target_none_is_ok() {
        let t = build_optional_target(None, None, &empty_target()).unwrap();
        assert!(t.is_none());
    }

    #[test]
    fn build_optional_target_with_coords() {
        let t = build_optional_target(Some(0), Some(0), &empty_target()).unwrap();
        assert!(matches!(t, Some(MouseTarget::Coords { col: 0, row: 0 })));
    }
}
