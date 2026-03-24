//! Maps human-readable key names to the byte sequences expected by a PTY.
//!
//! Examples:
//!   "Enter"      → \r
//!   "Ctrl+C"     → \x03
//!   "F1"         → \x1bOP
//!   "Shift+Up"   → \x1b[1;2A

/// Resolve a single key name (e.g. "Enter", "Ctrl+C", "F5") to its byte sequence.
pub fn resolve_key(name: &str) -> anyhow::Result<Vec<u8>> {
    let lower = name.to_lowercase();

    // Modifier combos: Ctrl+X, Alt+X, Shift+X, Ctrl+Shift+X, etc.
    if let Some(bytes) = resolve_modifier_combo(&lower) {
        return Ok(bytes);
    }

    // Single named keys
    if let Some(bytes) = resolve_named_key(&lower) {
        return Ok(bytes);
    }

    // Single printable character
    let chars: Vec<char> = name.chars().collect();
    if chars.len() == 1 {
        let mut buf = [0u8; 4];
        let s = chars[0].encode_utf8(&mut buf);
        return Ok(s.as_bytes().to_vec());
    }

    anyhow::bail!(
        "Unknown key: {name:?}. Valid keys: Enter, Tab, Escape, Space, Backspace, Delete, Insert, \
         Up, Down, Left, Right, Home, End, PageUp, PageDown, F1-F12, \
         Ctrl+<key>, Alt+<key>, Shift+<key>"
    );
}

/// Resolve a sequence of space-separated key names.
pub fn resolve_keys(names: &[String]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    for name in names {
        out.extend(resolve_key(name)?);
    }
    Ok(out)
}

fn resolve_named_key(name: &str) -> Option<Vec<u8>> {
    let bytes: &[u8] = match name {
        // Editing
        "enter" | "return" => b"\r",
        "tab" => b"\t",
        "escape" | "esc" => b"\x1b",
        "space" => b" ",
        "backspace" => b"\x7f",
        "delete" | "del" => b"\x1b[3~",
        "insert" | "ins" => b"\x1b[2~",

        // Navigation (SS3 sequences matching xterm-256color terminfo)
        "up" => b"\x1bOA",
        "down" => b"\x1bOB",
        "right" => b"\x1bOC",
        "left" => b"\x1bOD",
        "home" => b"\x1bOH",
        "end" => b"\x1bOF",
        "pageup" | "pgup" => b"\x1b[5~",
        "pagedown" | "pgdn" | "pgdown" => b"\x1b[6~",

        // Function keys
        "f1" => b"\x1bOP",
        "f2" => b"\x1bOQ",
        "f3" => b"\x1bOR",
        "f4" => b"\x1bOS",
        "f5" => b"\x1b[15~",
        "f6" => b"\x1b[17~",
        "f7" => b"\x1b[18~",
        "f8" => b"\x1b[19~",
        "f9" => b"\x1b[20~",
        "f10" => b"\x1b[21~",
        "f11" => b"\x1b[23~",
        "f12" => b"\x1b[24~",

        _ => return None,
    };
    Some(bytes.to_vec())
}

fn resolve_modifier_combo(name: &str) -> Option<Vec<u8>> {
    // Parse modifier+key patterns like "ctrl+c", "alt+f", "shift+tab", "ctrl+shift+up"
    let parts: Vec<&str> = name.split('+').collect();
    if parts.len() < 2 {
        return None;
    }

    let key = parts.last().unwrap();
    let modifiers = &parts[..parts.len() - 1];

    let mut has_ctrl = false;
    let mut has_alt = false;
    let mut has_shift = false;

    for m in modifiers {
        match *m {
            "ctrl" => has_ctrl = true,
            "alt" | "meta" => has_alt = true,
            "shift" => has_shift = true,
            _ => return None,
        }
    }

    // Ctrl+letter → control code (0x01-0x1a)
    if has_ctrl && !has_alt && !has_shift && key.len() == 1 {
        let ch = key.chars().next().unwrap();
        if ch.is_ascii_lowercase() {
            return Some(vec![ch as u8 - b'a' + 1]);
        }
    }

    // Alt+key → ESC prefix + key
    if has_alt && !has_ctrl && !has_shift {
        if key.len() == 1 {
            let ch = key.chars().next().unwrap();
            return Some(vec![0x1b, ch as u8]);
        }
        // Alt+named key → ESC prefix + key sequence
        if let Some(base) = resolve_named_key(key) {
            let mut out = vec![0x1b];
            out.extend(base);
            return Some(out);
        }
    }

    // Shift+Tab → backtab
    if has_shift && !has_ctrl && !has_alt && *key == "tab" {
        return Some(b"\x1b[Z".to_vec());
    }

    // Modified arrow/nav keys use xterm encoding: CSI 1;{mod}{letter}
    // mod: 2=Shift, 3=Alt, 4=Shift+Alt, 5=Ctrl, 6=Ctrl+Shift, 7=Ctrl+Alt, 8=Ctrl+Shift+Alt
    let modifier_code = match (has_ctrl, has_alt, has_shift) {
        (false, false, true) => 2,
        (false, true, false) => 3,
        (false, true, true) => 4,
        (true, false, false) => 5,
        (true, false, true) => 6,
        (true, true, false) => 7,
        (true, true, true) => 8,
        _ => return None,
    };

    // Arrow keys with modifiers: CSI 1;{mod}{A-D}
    let arrow_suffix = match *key {
        "up" => Some(b'A'),
        "down" => Some(b'B'),
        "right" => Some(b'C'),
        "left" => Some(b'D'),
        "home" => Some(b'H'),
        "end" => Some(b'F'),
        _ => None,
    };

    if let Some(suffix) = arrow_suffix {
        return Some(format!("\x1b[1;{modifier_code}{}", suffix as char).into_bytes());
    }

    // Function/nav keys with modifiers: CSI {code};{mod}~
    let tilde_code = match *key {
        "insert" | "ins" => Some(2),
        "delete" | "del" => Some(3),
        "pageup" | "pgup" => Some(5),
        "pagedown" | "pgdn" | "pgdown" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    };

    if let Some(code) = tilde_code {
        return Some(format!("\x1b[{code};{modifier_code}~").into_bytes());
    }

    // F1-F4 with modifiers: CSI 1;{mod}{P-S}
    let f1_4_suffix = match *key {
        "f1" => Some(b'P'),
        "f2" => Some(b'Q'),
        "f3" => Some(b'R'),
        "f4" => Some(b'S'),
        _ => None,
    };

    if let Some(suffix) = f1_4_suffix {
        return Some(format!("\x1b[1;{modifier_code}{}", suffix as char).into_bytes());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_keys() {
        assert_eq!(resolve_key("Enter").unwrap(), b"\r");
        assert_eq!(resolve_key("enter").unwrap(), b"\r");
        assert_eq!(resolve_key("Tab").unwrap(), b"\t");
        assert_eq!(resolve_key("Escape").unwrap(), b"\x1b");
        assert_eq!(resolve_key("Space").unwrap(), b" ");
        assert_eq!(resolve_key("Backspace").unwrap(), b"\x7f");
    }

    #[test]
    fn test_arrows() {
        assert_eq!(resolve_key("Up").unwrap(), b"\x1bOA");
        assert_eq!(resolve_key("Down").unwrap(), b"\x1bOB");
        assert_eq!(resolve_key("Right").unwrap(), b"\x1bOC");
        assert_eq!(resolve_key("Left").unwrap(), b"\x1bOD");
    }

    #[test]
    fn test_function_keys() {
        assert_eq!(resolve_key("F1").unwrap(), b"\x1bOP");
        assert_eq!(resolve_key("F5").unwrap(), b"\x1b[15~");
        assert_eq!(resolve_key("F12").unwrap(), b"\x1b[24~");
    }

    #[test]
    fn test_ctrl() {
        assert_eq!(resolve_key("Ctrl+C").unwrap(), vec![0x03]);
        assert_eq!(resolve_key("Ctrl+D").unwrap(), vec![0x04]);
        assert_eq!(resolve_key("Ctrl+Z").unwrap(), vec![0x1a]);
        assert_eq!(resolve_key("Ctrl+L").unwrap(), vec![0x0c]);
    }

    #[test]
    fn test_alt() {
        assert_eq!(resolve_key("Alt+f").unwrap(), vec![0x1b, b'f']);
    }

    #[test]
    fn test_shift_tab() {
        assert_eq!(resolve_key("Shift+Tab").unwrap(), b"\x1b[Z");
    }

    #[test]
    fn test_modified_arrows() {
        assert_eq!(resolve_key("Shift+Up").unwrap(), b"\x1b[1;2A");
        assert_eq!(resolve_key("Ctrl+Shift+Up").unwrap(), b"\x1b[1;6A");
    }

    #[test]
    fn test_single_char() {
        assert_eq!(resolve_key("a").unwrap(), b"a");
        assert_eq!(resolve_key("Z").unwrap(), b"Z");
        assert_eq!(resolve_key("!").unwrap(), b"!");
    }

    #[test]
    fn test_unknown_key() {
        assert!(resolve_key("FooBar").is_err());
    }

    #[test]
    fn test_resolve_keys_sequence() {
        let keys = vec!["Down".into(), "Down".into(), "Enter".into()];
        let result = resolve_keys(&keys).unwrap();
        assert_eq!(result, b"\x1bOB\x1bOB\r");
    }
}
