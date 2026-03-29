/// Terminal color value, decoupled from `vt100::Color` so it can be owned, cloned, and
/// used outside of a parser borrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// The terminal's default foreground or background (context-dependent).
    Default,
    /// An index into the xterm-256color palette (0..=255).
    Indexed(u8),
    /// A direct 24-bit RGB color.
    Rgb(u8, u8, u8),
}

impl From<vt100::Color> for Color {
    fn from(value: vt100::Color) -> Self {
        match value {
            vt100::Color::Default => Self::Default,
            vt100::Color::Idx(idx) => Self::Indexed(idx),
            vt100::Color::Rgb(r, g, b) => Self::Rgb(r, g, b),
        }
    }
}

/// SGR text attributes for a single cell.
///
/// `bold`, `italic`, and `underline` are captured for completeness but the image renderer
/// currently only acts on `inverse` (swapping foreground/background colors).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellAttributes {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

/// A single character cell extracted from the vt100 screen.
///
/// Wide (CJK) characters occupy two columns. The first column holds the character
/// content; the second is a continuation cell with `is_wide_continuation` set and an
/// empty `contents` string. The image renderer skips continuation cells to avoid
/// double-drawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub contents: String,
    pub fg: Color,
    pub bg: Color,
    pub attrs: CellAttributes,
    /// True for the trailing column of a wide character. The renderer must skip
    /// text drawing for these cells but still paint the background.
    pub is_wide_continuation: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            contents: String::new(),
            fg: Color::Default,
            bg: Color::Default,
            attrs: CellAttributes::default(),
            is_wide_continuation: false,
        }
    }
}

/// An owned, `Send`-safe snapshot of the vt100 emulator's visible screen.
///
/// This is the abstraction boundary between the daemon's parser (which holds a borrowed,
/// non-`Send` `vt100::Screen`) and the render pipeline. Construct via
/// [`ScreenSnapshot::from_vt100`], then pass to the text or image renderers.
///
/// The grid is stored row-major: `cells[row][col]`. Dimensions are guaranteed to match
/// the parser's size at the time of extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSnapshot {
    rows: u16,
    cols: u16,
    cells: Vec<Vec<Cell>>,
}

impl ScreenSnapshot {
    /// Extract an owned snapshot from a borrowed vt100 screen.
    ///
    /// Iterates every cell in the visible area and copies its content, colors, and
    /// attributes. Cells that the parser reports as `None` (which shouldn't happen
    /// for in-bounds coordinates) are replaced with [`Cell::default`].
    pub fn from_vt100(screen: &vt100::Screen) -> Self {
        let (rows, cols) = screen.size();
        let mut cells = Vec::with_capacity(rows as usize);

        for row in 0..rows {
            let mut row_cells = Vec::with_capacity(cols as usize);
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    row_cells.push(Cell::default());
                    continue;
                };

                row_cells.push(Cell {
                    contents: cell.contents().to_string(),
                    fg: cell.fgcolor().into(),
                    bg: cell.bgcolor().into(),
                    attrs: CellAttributes {
                        bold: cell.bold(),
                        italic: cell.italic(),
                        underline: cell.underline(),
                        inverse: cell.inverse(),
                    },
                    is_wide_continuation: cell.is_wide_continuation(),
                });
            }
            cells.push(row_cells);
        }

        Self { rows, cols, cells }
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn cells(&self) -> &[Vec<Cell>] {
        &self.cells
    }
}

#[cfg(test)]
mod tests {
    use super::{Color, ScreenSnapshot};

    #[test]
    fn builds_snapshot_from_vt100_screen() {
        let mut parser = vt100::Parser::new(4, 10, 0);
        parser.process(b"\x1b[31mA\x1b[7mB\x1b[0m");

        let snapshot = ScreenSnapshot::from_vt100(parser.screen());
        assert_eq!(snapshot.rows(), 4);
        assert_eq!(snapshot.cols(), 10);
        assert_eq!(snapshot.cells()[0][0].contents, "A");
        assert_eq!(snapshot.cells()[0][0].fg, Color::Indexed(1));
        assert_eq!(snapshot.cells()[0][1].contents, "B");
        assert!(snapshot.cells()[0][1].attrs.inverse);
    }
}
