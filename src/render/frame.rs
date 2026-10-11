//! Reusable terminal cells with optional foreground and background colors.

use crossterm::style::Color;

use super::color::write_sgr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub character: char,
    /// None means the terminal's default foreground.
    pub fg: Option<Color>,
    /// None preserves the terminal's own background, including transparency.
    pub bg: Option<Color>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            character: ' ',
            fg: None,
            bg: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
}

impl Frame {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            cells: vec![Cell::default(); usize::from(width) * usize::from(height)],
        }
    }

    pub const fn width(&self) -> u16 {
        self.width
    }

    pub const fn height(&self) -> u16 {
        self.height
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn cells_mut(&mut self) -> &mut [Cell] {
        &mut self.cells
    }

    pub fn cell(&self, x: u16, y: u16) -> Option<&Cell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.cells
            .get(usize::from(y) * usize::from(self.width) + usize::from(x))
    }

    pub fn cell_mut(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.cells
            .get_mut(usize::from(y) * usize::from(self.width) + usize::from(x))
    }

    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        if x < self.width && y < self.height {
            self.cells[usize::from(y) * usize::from(self.width) + usize::from(x)] = cell;
        }
    }

    pub fn clear(&mut self) {
        self.cells.fill(Cell::default());
    }

    /// Display an ASCII message without allocating, clipped to the frame width.
    pub fn message(&mut self, message: &str) {
        self.clear();
        let length = message.chars().count().min(usize::from(self.width));
        let x = (usize::from(self.width) - length) / 2;
        let y = self.height / 2;
        for (offset, character) in message.chars().take(length).enumerate() {
            self.set(
                (x + offset) as u16,
                y,
                Cell {
                    character,
                    ..Cell::default()
                },
            );
        }
    }

    /// The frame as text with ANSI colors, for headless snapshots. Every line
    /// ends with a reset so that nothing bleeds past it. Allocates; only for
    /// exporting the final headless frame.
    pub fn ansi_text(&self) -> String {
        let mut bytes = Vec::with_capacity(self.cells.len().saturating_mul(8));
        let (mut foreground, mut background) = (None, None);
        for row in self.cells.chunks(usize::from(self.width).max(1)) {
            for cell in row {
                // Writing to a Vec cannot fail.
                if cell.fg != foreground {
                    let _ = write_sgr(&mut bytes, cell.fg, true);
                    foreground = cell.fg;
                }
                if cell.bg != background {
                    let _ = write_sgr(&mut bytes, cell.bg, false);
                    background = cell.bg;
                }
                let mut encoded = [0; 4];
                bytes.extend_from_slice(cell.character.encode_utf8(&mut encoded).as_bytes());
            }
            if foreground.is_some() || background.is_some() {
                bytes.extend_from_slice(b"\x1b[0m");
                (foreground, background) = (None, None);
            }
            bytes.push(b'\n');
        }
        String::from_utf8(bytes).unwrap_or_default()
    }

    /// Allocate only when exporting the final headless frame.
    pub fn plain_text(&self) -> String {
        let mut text =
            String::with_capacity(self.cells.len().saturating_mul(3) + usize::from(self.height));
        for y in 0..self.height {
            let start = usize::from(y) * usize::from(self.width);
            for cell in &self.cells[start..start + usize::from(self.width)] {
                text.push(cell.character);
            }
            text.push('\n');
        }
        text
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn frame_reuses_storage_and_exports_exact_dimensions() {
        let mut frame = Frame::new(4, 2);
        let pointer = frame.cells().as_ptr();
        frame.set(
            1,
            0,
            Cell {
                character: '⣿',
                ..Cell::default()
            },
        );
        assert_eq!(frame.plain_text(), " ⣿  \n    \n");
        assert!(frame.cell(4, 0).is_none());
        assert!(frame.cell(0, 2).is_none());
        frame.clear();
        assert_eq!(frame.cells().as_ptr(), pointer);
        assert!(frame.cells().iter().all(|cell| *cell == Cell::default()));
    }

    #[test]
    fn small_frame_message_is_centered_and_clipped() {
        let mut frame = Frame::new(6, 3);
        frame.message("Terminal too small");
        assert_eq!(frame.plain_text(), "      \nTermin\n      \n");
        Frame::new(0, 0).message("Terminal too small");
    }

    #[test]
    fn ansi_text_sets_colors_once_per_run_and_resets_open_styles_at_line_ends() {
        let mut frame = Frame::new(3, 2);
        for x in 0..2 {
            frame.set(
                x,
                0,
                Cell {
                    character: 'x',
                    fg: Some(Color::AnsiValue(1)),
                    bg: None,
                },
            );
        }
        frame.set(
            2,
            1,
            Cell {
                character: 'y',
                fg: None,
                bg: Some(Color::Rgb { r: 1, g: 2, b: 3 }),
            },
        );
        assert_eq!(
            frame.ansi_text(),
            "\x1b[31mxx\x1b[39m \n  \x1b[48;2;1;2;3my\x1b[0m\n"
        );
        // Without any color the text is exactly the plain text.
        let plain = Frame::new(2, 2);
        assert_eq!(plain.ansi_text(), plain.plain_text());
    }
}
