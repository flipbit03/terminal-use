#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellAttributes {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub contents: String,
    pub fg: Color,
    pub bg: Color,
    pub attrs: CellAttributes,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSnapshot {
    rows: u16,
    cols: u16,
    cells: Vec<Vec<Cell>>,
}

impl ScreenSnapshot {
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
