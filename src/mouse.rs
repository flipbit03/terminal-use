//! Mouse-event encoding for xterm-style mouse protocols, plus helpers to
//! resolve text/regex targets against the visible screen.
//!
//! Wire format support: SGR (DECSET 1006), Default legacy (DECSET 1000), and
//! UTF-8 (DECSET 1005). The inner application's `DECSET 100x` enables a mode;
//! `vt100`'s `screen.mouse_protocol_mode()` / `mouse_protocol_encoding()` tells
//! us which one to emit.
//!
//! Coordinates on the public/CLI surface are 0-based (matching `cursor` /
//! `screenshot`). Wire formats are 1-based; we add 1 at encode time.

use anyhow::{anyhow, bail, Result};
use regex::Regex;

use crate::daemon::protocol::{MouseButton, MouseEncoding, MouseMods, ScrollDir};

/// One low-level wire event ready to encode.
#[derive(Debug, Clone, Copy)]
pub enum WireEvent {
    /// Button press at (col, row), 0-based.
    Down {
        col: u16,
        row: u16,
        button: MouseButton,
        mods: MouseMods,
    },
    /// Button release at (col, row), 0-based.
    Up {
        col: u16,
        row: u16,
        button: MouseButton,
        mods: MouseMods,
    },
    /// Bare move (AnyMotion) — no button held.
    Move { col: u16, row: u16, mods: MouseMods },
    /// Move while button is held (drag).
    DragMove {
        col: u16,
        row: u16,
        button: MouseButton,
        mods: MouseMods,
    },
    /// Wheel notch.
    Scroll {
        col: u16,
        row: u16,
        dir: ScrollDir,
        mods: MouseMods,
    },
}

fn button_low_bits(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

fn mods_bits(mods: MouseMods) -> u32 {
    let mut b = 0u32;
    if mods.shift {
        b |= 4;
    }
    if mods.alt {
        b |= 8;
    }
    if mods.ctrl {
        b |= 16;
    }
    b
}

fn scroll_low_bits(dir: ScrollDir) -> u32 {
    // Scroll events use the high bit (64) plus a direction code.
    match dir {
        ScrollDir::Up => 0,
        ScrollDir::Down => 1,
        ScrollDir::Left => 2,
        ScrollDir::Right => 3,
    }
}

/// Compute the SGR `<button>` parameter (which is also the byte-32 base for
/// legacy encodings). Bit 5 (32) marks motion; bit 6 (64) marks wheel.
fn sgr_button(event: &WireEvent) -> u32 {
    match event {
        WireEvent::Down { button, mods, .. } | WireEvent::Up { button, mods, .. } => {
            button_low_bits(*button) | mods_bits(*mods)
        }
        WireEvent::Move { mods, .. } => {
            // bare move: motion bit + "no button" sentinel (3 in low bits).
            32 | 3 | mods_bits(*mods)
        }
        WireEvent::DragMove { button, mods, .. } => {
            32 | button_low_bits(*button) | mods_bits(*mods)
        }
        WireEvent::Scroll { dir, mods, .. } => 64 | scroll_low_bits(*dir) | mods_bits(*mods),
    }
}

fn coords(event: &WireEvent) -> (u16, u16) {
    match event {
        WireEvent::Down { col, row, .. }
        | WireEvent::Up { col, row, .. }
        | WireEvent::Move { col, row, .. }
        | WireEvent::DragMove { col, row, .. }
        | WireEvent::Scroll { col, row, .. } => (*col, *row),
    }
}

fn is_release(event: &WireEvent) -> bool {
    matches!(event, WireEvent::Up { .. })
}

/// Encode a sequence of events into bytes ready to write to the PTY master.
///
/// Errors if the chosen encoding cannot represent the event (e.g. legacy
/// encoding tops out at col/row 223).
pub fn encode(events: &[WireEvent], encoding: MouseEncoding) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(events.len() * 12);
    for ev in events {
        match encoding {
            MouseEncoding::Sgr => encode_sgr(ev, &mut out),
            MouseEncoding::Default => encode_default(ev, &mut out)?,
            MouseEncoding::Utf8 => encode_utf8(ev, &mut out)?,
        }
    }
    Ok(out)
}

fn encode_sgr(event: &WireEvent, out: &mut Vec<u8>) {
    let (col, row) = coords(event);
    let cb = sgr_button(event);
    let final_byte = if is_release(event) { 'm' } else { 'M' };
    out.extend_from_slice(
        format!(
            "\x1b[<{};{};{}{}",
            cb,
            col as u32 + 1,
            row as u32 + 1,
            final_byte
        )
        .as_bytes(),
    );
}

/// Legacy `CSI M Cb Cx Cy` encoding (DECSET 1000/1002 without SGR).
///
/// Each parameter is a single byte = `value + 32`. Releases are encoded with
/// button code `3` (the protocol can't distinguish *which* button was released).
fn encode_default(event: &WireEvent, out: &mut Vec<u8>) -> Result<()> {
    let (col, row) = coords(event);
    let cb = if is_release(event) {
        // Legacy: any release becomes button 3, plus modifiers.
        3 | match event {
            WireEvent::Up { mods, .. } => mods_bits(*mods),
            _ => 0,
        }
    } else {
        sgr_button(event)
    };

    let cx = col as u32 + 1;
    let cy = row as u32 + 1;
    if cx > 223 || cy > 223 {
        bail!(
            "legacy mouse encoding cannot represent col={} row={} (max 223). \
             The inner app should enable SGR (DECSET 1006).",
            col,
            row
        );
    }
    if cb > 223 {
        bail!("legacy mouse encoding overflowed the button byte");
    }

    out.extend_from_slice(b"\x1b[M");
    out.push((cb + 32) as u8);
    out.push((cx + 32) as u8);
    out.push((cy + 32) as u8);
    Ok(())
}

/// UTF-8 encoding (DECSET 1005): same as legacy but Cx/Cy may exceed 223 by
/// being encoded as UTF-8 codepoints. Cb is still a single byte.
fn encode_utf8(event: &WireEvent, out: &mut Vec<u8>) -> Result<()> {
    let (col, row) = coords(event);
    let cb = if is_release(event) {
        3 | match event {
            WireEvent::Up { mods, .. } => mods_bits(*mods),
            _ => 0,
        }
    } else {
        sgr_button(event)
    };

    if cb > 223 {
        bail!("UTF-8 mouse encoding overflowed the button byte");
    }

    out.extend_from_slice(b"\x1b[M");
    out.push((cb + 32) as u8);
    push_utf8_coord(out, col as u32 + 1)?;
    push_utf8_coord(out, row as u32 + 1)?;
    Ok(())
}

fn push_utf8_coord(out: &mut Vec<u8>, value: u32) -> Result<()> {
    let cp = value
        .checked_add(32)
        .ok_or_else(|| anyhow!("mouse coordinate overflow"))?;
    let ch = char::from_u32(cp).ok_or_else(|| anyhow!("invalid utf-8 mouse coordinate {cp}"))?;
    let mut buf = [0u8; 4];
    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    Ok(())
}

/// Parse a comma-separated modifier string (`"Ctrl,Shift"`, case-insensitive).
pub fn parse_mods(s: &str) -> Result<MouseMods> {
    let mut mods = MouseMods::default();
    if s.is_empty() {
        return Ok(mods);
    }
    for part in s.split(',') {
        let token = part.trim().to_lowercase();
        match token.as_str() {
            "" => continue,
            "shift" => mods.shift = true,
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "meta" => mods.alt = true,
            "super" | "hyper" | "cmd" | "command" | "win" => {
                bail!(
                    "modifier {part:?} is not representable in the xterm mouse protocol \
                     (only Ctrl, Shift, Alt are supported)"
                )
            }
            _ => bail!("unknown modifier {part:?}. Valid: Ctrl, Shift, Alt"),
        }
    }
    Ok(mods)
}

/// Parse `--button left|right|middle` (case-insensitive).
pub fn parse_button(s: &str) -> Result<MouseButton, String> {
    match s.to_lowercase().as_str() {
        "left" | "l" => Ok(MouseButton::Left),
        "right" | "r" => Ok(MouseButton::Right),
        "middle" | "m" => Ok(MouseButton::Middle),
        _ => Err(format!("invalid --button {s:?}. Use left|right|middle")),
    }
}

/// Parse `up|down|left|right` for scroll direction.
pub fn parse_scroll_dir(s: &str) -> Result<ScrollDir, String> {
    match s.to_lowercase().as_str() {
        "up" | "u" => Ok(ScrollDir::Up),
        "down" | "d" => Ok(ScrollDir::Down),
        "left" | "l" => Ok(ScrollDir::Left),
        "right" | "r" => Ok(ScrollDir::Right),
        _ => Err(format!(
            "invalid scroll direction {s:?}. Use up|down|left|right"
        )),
    }
}

/// One match found on the visible screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenMatch {
    pub row: u16,
    pub col_start: u16,
    pub col_end: u16, // exclusive
}

impl ScreenMatch {
    pub fn center(self) -> (u16, u16) {
        let len = self.col_end.saturating_sub(self.col_start);
        let mid = self.col_start + len / 2;
        (mid, self.row)
    }
}

/// Find every line-confined occurrence of `needle` in the rendered screen.
///
/// `screen_rows` is one string per visible row (no trailing newline).
pub fn find_text(screen_rows: &[String], needle: &str) -> Vec<ScreenMatch> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (row_idx, line) in screen_rows.iter().enumerate() {
        let mut search_from = 0usize;
        while let Some(byte_idx) = line[search_from..].find(needle) {
            let start_byte = search_from + byte_idx;
            let end_byte = start_byte + needle.len();
            // Convert byte offset to char column. Visible cells map 1:1 to chars
            // because vt100 emits each cell as one character.
            let col_start = line[..start_byte].chars().count() as u16;
            let col_end = line[..end_byte].chars().count() as u16;
            out.push(ScreenMatch {
                row: row_idx as u16,
                col_start,
                col_end,
            });
            search_from = end_byte.max(start_byte + 1);
        }
    }
    out
}

/// Find every regex match in the rendered screen, line by line.
pub fn find_regex(screen_rows: &[String], pattern: &str) -> Result<Vec<ScreenMatch>> {
    let re = Regex::new(pattern).map_err(|e| anyhow!("invalid regex {pattern:?}: {e}"))?;
    let mut out = Vec::new();
    for (row_idx, line) in screen_rows.iter().enumerate() {
        for m in re.find_iter(line) {
            let col_start = line[..m.start()].chars().count() as u16;
            let col_end = line[..m.end()].chars().count() as u16;
            out.push(ScreenMatch {
                row: row_idx as u16,
                col_start,
                col_end,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dn(col: u16, row: u16) -> WireEvent {
        WireEvent::Down {
            col,
            row,
            button: MouseButton::Left,
            mods: MouseMods::default(),
        }
    }
    fn up(col: u16, row: u16) -> WireEvent {
        WireEvent::Up {
            col,
            row,
            button: MouseButton::Left,
            mods: MouseMods::default(),
        }
    }

    #[test]
    fn sgr_left_click_press_then_release() {
        let bytes = encode(&[dn(49, 19), up(49, 19)], MouseEncoding::Sgr).unwrap();
        // 0-based 49,19 → 1-based 50,20. Left = 0.
        assert_eq!(bytes, b"\x1b[<0;50;20M\x1b[<0;50;20m");
    }

    #[test]
    fn sgr_right_button_with_ctrl_shift() {
        let ev = WireEvent::Down {
            col: 0,
            row: 0,
            button: MouseButton::Right,
            mods: MouseMods {
                shift: true,
                alt: false,
                ctrl: true,
            },
        };
        // right=2, shift=4, ctrl=16 → 22; coords 1,1 (1-based)
        let bytes = encode(&[ev], MouseEncoding::Sgr).unwrap();
        assert_eq!(bytes, b"\x1b[<22;1;1M");
    }

    #[test]
    fn sgr_middle_button() {
        let ev = WireEvent::Down {
            col: 9,
            row: 4,
            button: MouseButton::Middle,
            mods: MouseMods::default(),
        };
        let bytes = encode(&[ev], MouseEncoding::Sgr).unwrap();
        assert_eq!(bytes, b"\x1b[<1;10;5M");
    }

    #[test]
    fn sgr_drag_move_sets_motion_bit() {
        let ev = WireEvent::DragMove {
            col: 4,
            row: 4,
            button: MouseButton::Left,
            mods: MouseMods::default(),
        };
        // motion(32) | left(0) = 32
        let bytes = encode(&[ev], MouseEncoding::Sgr).unwrap();
        assert_eq!(bytes, b"\x1b[<32;5;5M");
    }

    #[test]
    fn sgr_bare_move_uses_button_3() {
        let ev = WireEvent::Move {
            col: 0,
            row: 0,
            mods: MouseMods::default(),
        };
        // motion(32) | nobutton(3) = 35
        let bytes = encode(&[ev], MouseEncoding::Sgr).unwrap();
        assert_eq!(bytes, b"\x1b[<35;1;1M");
    }

    #[test]
    fn sgr_scroll_directions() {
        let mk = |dir| WireEvent::Scroll {
            col: 0,
            row: 0,
            dir,
            mods: MouseMods::default(),
        };
        let up = encode(&[mk(ScrollDir::Up)], MouseEncoding::Sgr).unwrap();
        let down = encode(&[mk(ScrollDir::Down)], MouseEncoding::Sgr).unwrap();
        let left = encode(&[mk(ScrollDir::Left)], MouseEncoding::Sgr).unwrap();
        let right = encode(&[mk(ScrollDir::Right)], MouseEncoding::Sgr).unwrap();
        assert_eq!(up, b"\x1b[<64;1;1M");
        assert_eq!(down, b"\x1b[<65;1;1M");
        assert_eq!(left, b"\x1b[<66;1;1M");
        assert_eq!(right, b"\x1b[<67;1;1M");
    }

    #[test]
    fn default_legacy_encoding_press() {
        let bytes = encode(&[dn(0, 0)], MouseEncoding::Default).unwrap();
        // Cb=0+32=32 (' '), Cx=1+32=33 ('!'), Cy=1+32=33 ('!')
        assert_eq!(bytes, b"\x1b[M !!");
    }

    #[test]
    fn default_legacy_release_uses_button_3() {
        let bytes = encode(&[up(0, 0)], MouseEncoding::Default).unwrap();
        // Cb = 3 + 32 = 35 ('#')
        assert_eq!(bytes, b"\x1b[M#!!");
    }

    #[test]
    fn default_encoding_rejects_overflow() {
        let err = encode(&[dn(300, 0)], MouseEncoding::Default).unwrap_err();
        assert!(err.to_string().contains("legacy"));
    }

    #[test]
    fn utf8_encoding_handles_high_coords() {
        // col=300 → 301 → 333 codepoint = ǌ-ish; just round-trip the bytes.
        let bytes = encode(&[dn(300, 0)], MouseEncoding::Utf8).unwrap();
        // First three bytes are CSI M, then Cb=' ', then UTF-8 of (300+1+32)=333,
        // then UTF-8 of (0+1+32)=33.
        assert!(bytes.starts_with(b"\x1b[M "));
        let mut buf = [0u8; 4];
        let cx_bytes = char::from_u32(333).unwrap().encode_utf8(&mut buf).len();
        assert_eq!(
            &bytes[4..4 + cx_bytes],
            char::from_u32(333).unwrap().to_string().as_bytes()
        );
        assert_eq!(bytes[4 + cx_bytes], b'!');
    }

    #[test]
    fn parse_mods_accepts_known_combos() {
        assert_eq!(parse_mods("").unwrap(), MouseMods::default());
        let m = parse_mods("Ctrl,Shift").unwrap();
        assert!(m.ctrl && m.shift && !m.alt);
        let m = parse_mods("alt").unwrap();
        assert!(m.alt);
        let m = parse_mods("CTRL, ALT , SHIFT").unwrap();
        assert!(m.ctrl && m.alt && m.shift);
    }

    #[test]
    fn parse_mods_rejects_unknown() {
        assert!(parse_mods("Hyper").is_err());
        assert!(parse_mods("super").is_err());
        assert!(parse_mods("foobar").is_err());
    }

    #[test]
    fn parse_button_variants() {
        assert_eq!(parse_button("left").unwrap(), MouseButton::Left);
        assert_eq!(parse_button("RIGHT").unwrap(), MouseButton::Right);
        assert_eq!(parse_button("Middle").unwrap(), MouseButton::Middle);
        assert!(parse_button("foo").is_err());
    }

    #[test]
    fn screen_match_center_handles_odd_and_even_widths() {
        assert_eq!(
            ScreenMatch {
                row: 3,
                col_start: 10,
                col_end: 13
            }
            .center(),
            (11, 3)
        );
        // even: 10..14 (length 4) → midpoint at 10 + 4/2 = 12
        assert_eq!(
            ScreenMatch {
                row: 3,
                col_start: 10,
                col_end: 14
            }
            .center(),
            (12, 3)
        );
    }

    #[test]
    fn find_text_basic() {
        let rows = vec![
            "  Buy upgrade  ".to_string(),
            "no match here".to_string(),
            "  Buy upgrade".to_string(),
        ];
        let hits = find_text(&rows, "Buy upgrade");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].row, 0);
        assert_eq!(hits[0].col_start, 2);
        assert_eq!(hits[0].col_end, 13);
        assert_eq!(hits[1].row, 2);
    }

    #[test]
    fn find_text_overlapping_does_not_loop() {
        let rows = vec!["aaaa".to_string()];
        let hits = find_text(&rows, "aa");
        // Non-overlapping at byte offsets 0, 2.
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn find_text_empty_needle_returns_empty() {
        let rows = vec!["hello".to_string()];
        assert!(find_text(&rows, "").is_empty());
    }

    #[test]
    fn find_regex_extracts_matches_per_line() {
        let rows = vec![
            "Buy 10 carrots".to_string(),
            "Buy 250 turnips".to_string(),
            "Sell 5".to_string(),
        ];
        let hits = find_regex(&rows, r"Buy\s+\d+").unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].col_start, 0);
        assert_eq!(hits[1].row, 1);
    }
}
