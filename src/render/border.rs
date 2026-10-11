//! Borders: a box of line-drawing characters around the plate, or for a drum
//! a ring drawn with the plate's own glyphs.

use crossterm::style::Color;

use crate::config::schema::{BorderStyle, TitleAlign};

use super::{
    frame::{Cell, Frame},
    glyphs::GlyphSet,
    layout::PlateLayout,
};

/// A rectangle of terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    /// The plate's rectangle grown by one cell on every side, saturating at
    /// the terminal's top-left corner.
    pub fn around(layout: &PlateLayout) -> Self {
        Self {
            x: layout.x.saturating_sub(1),
            y: layout.y.saturating_sub(1),
            width: layout.width + 2,
            height: layout.height + 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderChars {
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub horizontal: char,
    pub vertical: char,
}

/// The characters of a box style, or `None` for styles that draw no box.
/// Without Unicode (the Linux console font, a non-UTF-8 locale) every box is
/// drawn with `+`, `-` and `|`.
pub fn box_chars(style: BorderStyle, unicode: bool) -> Option<BorderChars> {
    if matches!(style, BorderStyle::None | BorderStyle::Circle) {
        return None;
    }
    if !unicode {
        return Some(BorderChars {
            top_left: '+',
            top_right: '+',
            bottom_left: '+',
            bottom_right: '+',
            horizontal: '-',
            vertical: '|',
        });
    }
    let [
        top_left,
        top_right,
        bottom_left,
        bottom_right,
        horizontal,
        vertical,
    ] = match style {
        BorderStyle::Rounded => ['╭', '╮', '╰', '╯', '─', '│'],
        BorderStyle::Square => ['┌', '┐', '└', '┘', '─', '│'],
        BorderStyle::Double => ['╔', '╗', '╚', '╝', '═', '║'],
        BorderStyle::Thick => ['┏', '┓', '┗', '┛', '━', '┃'],
        BorderStyle::None | BorderStyle::Circle => return None,
    };
    Some(BorderChars {
        top_left,
        top_right,
        bottom_left,
        bottom_right,
        horizontal,
        vertical,
    })
}

fn put(frame: &mut Frame, x: u16, y: u16, character: char, fg: Option<Color>) {
    if let Some(cell) = frame.cell_mut(x, y) {
        *cell = Cell {
            character,
            fg,
            bg: cell.bg,
        };
    }
}

/// Draw the outline of `rect`; rectangles smaller than 2x2 draw nothing.
pub fn draw_box(frame: &mut Frame, rect: Rect, chars: BorderChars, fg: Option<Color>) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let (right, bottom) = (rect.x + rect.width - 1, rect.y + rect.height - 1);
    for x in rect.x + 1..right {
        put(frame, x, rect.y, chars.horizontal, fg);
        put(frame, x, bottom, chars.horizontal, fg);
    }
    for y in rect.y + 1..bottom {
        put(frame, rect.x, y, chars.vertical, fg);
        put(frame, right, y, chars.vertical, fg);
    }
    put(frame, rect.x, rect.y, chars.top_left, fg);
    put(frame, right, rect.y, chars.top_right, fg);
    put(frame, rect.x, bottom, chars.bottom_left, fg);
    put(frame, right, bottom, chars.bottom_right, fg);
}

/// Write `text` into row `y` between columns `x` and `x + width`, aligned.
/// Text that does not fit is cut at the end. Returns the columns written.
pub fn draw_aligned(
    frame: &mut Frame,
    x: u16,
    y: u16,
    width: u16,
    text: &str,
    align: TitleAlign,
    fg: Option<Color>,
) -> u16 {
    let length = text.chars().count().min(usize::from(width)) as u16;
    let start = match align {
        TitleAlign::Left => 0,
        TitleAlign::Center => (width - length) / 2,
        TitleAlign::Right => width - length,
    };
    for (offset, character) in text.chars().take(usize::from(length)).enumerate() {
        put(frame, x + start + offset as u16, y, character, fg);
    }
    length
}

/// Draw the title over the top edge of a box, keeping the corners and one
/// line segment on each side clear.
pub fn draw_box_title(
    frame: &mut Frame,
    rect: Rect,
    title: &str,
    align: TitleAlign,
    fg: Option<Color>,
) {
    if rect.width > 4 {
        draw_aligned(frame, rect.x + 2, rect.y, rect.width - 4, title, align, fg);
    }
}

/// Glyph bits of a one-sub-pixel ring along the rim of the drum, per plate
/// cell. The ring is the ellipse that touches the plate's four sides, which is
/// where the sand is held, so a drum looks as if it sat in a hoop.
pub fn circle_ring(layout: &PlateLayout, glyphs: GlyphSet) -> Vec<u8> {
    let (dots_x, dots_y) = glyphs.subpixels();
    let (width, height) = (layout.subpixel_width as f32, layout.subpixel_height as f32);
    // One sub-pixel of thickness, a little more so that diagonals stay closed.
    let half_width = 0.8 / (width.min(height) / 2.0).max(1.0);
    let mut bits = vec![0_u8; usize::from(layout.width) * usize::from(layout.height)];
    for cell_y in 0..usize::from(layout.height) {
        for cell_x in 0..usize::from(layout.width) {
            for dy in 0..dots_y {
                for dx in 0..dots_x {
                    let px = ((cell_x * dots_x + dx) as f32 + 0.5 - width / 2.0) / (width / 2.0);
                    let py = ((cell_y * dots_y + dy) as f32 + 0.5 - height / 2.0) / (height / 2.0);
                    if (px.hypot(py) - 1.0).abs() <= half_width {
                        bits[cell_y * usize::from(layout.width) + cell_x] |= glyphs.bit(dx, dy);
                    }
                }
            }
        }
    }
    bits
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{config::schema::LayoutConfig, render::layout::Margins};

    fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn every_box_style_has_its_own_characters_and_ascii_has_a_fallback() {
        let chars = |style| box_chars(style, true).unwrap();
        assert_eq!(chars(BorderStyle::Rounded).top_left, '╭');
        assert_eq!(chars(BorderStyle::Square).top_left, '┌');
        assert_eq!(chars(BorderStyle::Double).horizontal, '═');
        assert_eq!(chars(BorderStyle::Thick).vertical, '┃');
        assert!(box_chars(BorderStyle::None, true).is_none());
        assert!(box_chars(BorderStyle::Circle, true).is_none());
        for style in [
            BorderStyle::Rounded,
            BorderStyle::Square,
            BorderStyle::Double,
            BorderStyle::Thick,
        ] {
            let ascii = box_chars(style, false).unwrap();
            assert_eq!(
                [ascii.top_left, ascii.horizontal, ascii.vertical],
                ['+', '-', '|']
            );
        }
    }

    #[test]
    fn a_box_is_drawn_on_the_outline_only_and_keeps_the_background() {
        let mut frame = Frame::new(6, 4);
        frame.cell_mut(2, 1).unwrap().character = 'x';
        frame.cell_mut(0, 0).unwrap().bg = Some(Color::Blue);
        draw_box(
            &mut frame,
            rect(0, 0, 6, 4),
            box_chars(BorderStyle::Rounded, true).unwrap(),
            Some(Color::Red),
        );
        assert_eq!(frame.plain_text(), "╭────╮\n│ x  │\n│    │\n╰────╯\n");
        assert_eq!(frame.cell(0, 0).unwrap().bg, Some(Color::Blue));
        assert_eq!(frame.cell(3, 0).unwrap().fg, Some(Color::Red));
        assert_eq!(frame.cell(2, 1).unwrap().fg, None);
        // A degenerate or partly outside rectangle never panics.
        draw_box(
            &mut frame,
            rect(0, 0, 1, 1),
            box_chars(BorderStyle::Square, true).unwrap(),
            None,
        );
        draw_box(
            &mut frame,
            rect(4, 3, 10, 10),
            box_chars(BorderStyle::Square, true).unwrap(),
            None,
        );
    }

    #[test]
    fn titles_align_inside_the_top_edge_and_are_cut_to_fit() {
        let title_in = |align, title: &str| {
            let mut frame = Frame::new(12, 3);
            let chars = box_chars(BorderStyle::Square, true).unwrap();
            draw_box(&mut frame, rect(0, 0, 12, 3), chars, None);
            draw_box_title(&mut frame, rect(0, 0, 12, 3), title, align, None);
            frame.plain_text().lines().next().unwrap().to_owned()
        };
        assert_eq!(title_in(TitleAlign::Left, " A4 "), "┌─ A4 ─────┐");
        assert_eq!(title_in(TitleAlign::Center, " A4 "), "┌─── A4 ───┐");
        assert_eq!(title_in(TitleAlign::Right, " A4 "), "┌───── A4 ─┐");
        // Too long: cut, never overwriting the corners.
        assert_eq!(
            title_in(TitleAlign::Left, "0123456789ABCDEF"),
            "┌─01234567─┐"
        );
        // A box too narrow for a title keeps its edge.
        let mut frame = Frame::new(4, 3);
        draw_box(
            &mut frame,
            rect(0, 0, 4, 3),
            box_chars(BorderStyle::Square, true).unwrap(),
            None,
        );
        draw_box_title(
            &mut frame,
            rect(0, 0, 4, 3),
            "long",
            TitleAlign::Center,
            None,
        );
        assert_eq!(frame.plain_text().lines().next().unwrap(), "┌──┐");
    }

    #[test]
    fn a_box_surrounds_the_plate_one_cell_out() {
        let layout = PlateLayout::new(
            40,
            20,
            &LayoutConfig::default(),
            0.5,
            GlyphSet::Braille,
            Margins::all(1),
        )
        .unwrap();
        let around = Rect::around(&layout);
        assert_eq!(
            (around.x, around.y, around.width, around.height),
            (
                layout.x - 1,
                layout.y - 1,
                layout.width + 2,
                layout.height + 2
            )
        );
        // The margins reserved for it keep the box inside the padding.
        assert!(around.x >= 2 && around.y >= 1);
        assert!(around.x + around.width <= 40 - 2 && around.y + around.height < 20);
    }

    #[test]
    fn the_circular_ring_is_closed_symmetric_and_inside_the_plate() {
        for glyphs in [
            GlyphSet::Braille,
            GlyphSet::Quadrant,
            GlyphSet::Half,
            GlyphSet::Ascii,
        ] {
            let layout = PlateLayout::new(
                60,
                32,
                &LayoutConfig::default(),
                0.5,
                glyphs,
                Margins::default(),
            )
            .unwrap();
            let ring = circle_ring(&layout, glyphs);
            let (w, h) = (usize::from(layout.width), usize::from(layout.height));
            assert_eq!(ring.len(), w * h);
            let lit = |x: usize, y: usize| ring[y * w + x] != 0;
            // The centre is empty; the four extremes of the circle are lit.
            assert!(!lit(w / 2, h / 2), "{glyphs:?}");
            assert!(lit(w / 2, 0) && lit(w / 2, h - 1), "{glyphs:?}");
            assert!(lit(0, h / 2) && lit(w - 1, h / 2), "{glyphs:?}");
            // Corners of the square are outside the drum.
            assert!(!lit(0, 0) && !lit(w - 1, h - 1), "{glyphs:?}");
            // Left-right and up-down mirror images, as a circle must be.
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(
                        lit(x, y),
                        lit(w - 1 - x, y),
                        "{glyphs:?} mirror x at {x},{y}"
                    );
                    assert_eq!(
                        lit(x, y),
                        lit(x, h - 1 - y),
                        "{glyphs:?} mirror y at {x},{y}"
                    );
                }
            }
            // The ring is continuous: every lit cell touches another lit cell.
            for y in 0..h {
                for x in 0..w {
                    if lit(x, y) {
                        let neighbours = (y.saturating_sub(1)..=(y + 1).min(h - 1))
                            .flat_map(|ny| {
                                (x.saturating_sub(1)..=(x + 1).min(w - 1)).map(move |nx| (nx, ny))
                            })
                            .filter(|&(nx, ny)| (nx, ny) != (x, y) && lit(nx, ny))
                            .count();
                        assert!(neighbours >= 1, "{glyphs:?} isolated ring cell {x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn ring_bits_use_the_glyph_sets_own_bit_layout() {
        let layout = PlateLayout::new(
            30,
            16,
            &LayoutConfig::default(),
            0.5,
            GlyphSet::Braille,
            Margins::default(),
        )
        .unwrap();
        let ring = circle_ring(&layout, GlyphSet::Braille);
        assert!(ring.iter().any(|&bits| bits != 0 && bits != 0xff));
        let half = circle_ring(&layout, GlyphSet::Half);
        assert!(half.iter().all(|&bits| bits <= 0x03));
        let quadrant = circle_ring(&layout, GlyphSet::Quadrant);
        assert!(quadrant.iter().all(|&bits| bits <= 0x0f));
    }
}
