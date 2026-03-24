use std::io::IsTerminal;

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
}

/// Resolve the output format: explicit flag > auto-detect from TTY.
pub fn resolve_format(json_flag: bool) -> Format {
    if json_flag {
        Format::Json
    } else if std::io::stdout().is_terminal() {
        Format::Human
    } else {
        Format::Json
    }
}
