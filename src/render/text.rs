use crate::daemon::protocol::CursorPos;

/// Format a plain text screenshot for human display.
///
/// `mouse_cursor` (when set) is reported as a trailer line *below* the body —
/// the body itself stays byte-identical to what the inner app drew so regex /
/// grep over `content` always matches the application's output, never tu's
/// synthetic overlay.
pub fn format_screenshot(
    content: &str,
    _rows: u16,
    _cols: u16,
    _cursor_row: u16,
    _cursor_col: u16,
    mouse_cursor: Option<CursorPos>,
    mouse_held: bool,
) -> String {
    match mouse_cursor {
        None => content.to_string(),
        Some(pos) => {
            let glyph = if mouse_held { "▲" } else { "△" };
            let label = if mouse_held { "held " } else { "" };
            format!(
                "{content}\n\n{glyph} tu mouse cursor {label}at ({col},{row})",
                col = pos.col,
                row = pos.row
            )
        }
    }
}

/// Format a screenshot as JSON.
pub fn format_screenshot_json(
    content: &str,
    rows: u16,
    cols: u16,
    cursor_row: u16,
    cursor_col: u16,
    mouse_cursor: Option<CursorPos>,
    mouse_held: bool,
) -> serde_json::Value {
    serde_json::json!({
        "type": "screenshot",
        "format": "text",
        "content": content,
        "rows": rows,
        "cols": cols,
        "cursor": {
            "row": cursor_row,
            "col": cursor_col,
        },
        "mouse_cursor": mouse_cursor.map(|p| serde_json::json!({"col": p.col, "row": p.row})),
        "mouse_held": mouse_held,
    })
}
