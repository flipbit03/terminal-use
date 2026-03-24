/// Format a plain text screenshot for human display.
pub fn format_screenshot(
    content: &str,
    _rows: u16,
    _cols: u16,
    _cursor_row: u16,
    _cursor_col: u16,
) -> String {
    content.to_string()
}

/// Format a screenshot as JSON.
pub fn format_screenshot_json(
    content: &str,
    rows: u16,
    cols: u16,
    cursor_row: u16,
    cursor_col: u16,
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
        }
    })
}
