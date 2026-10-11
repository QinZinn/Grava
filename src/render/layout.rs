//! Fit a physically square plate into terminal cell geometry.

use crate::config::schema::{Alignment, Fit, LayoutConfig};

use super::GlyphSet;

/// Cells kept free around the plate for the HUD row and the border ring, on
/// top of the configured padding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Margins {
    pub top: u16,
    pub bottom: u16,
    pub left: u16,
    pub right: u16,
}

impl Margins {
    pub const fn all(cells: u16) -> Self {
        Self {
            top: cells,
            bottom: cells,
            left: cells,
            right: cells,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlateLayout {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub subpixel_width: usize,
    pub subpixel_height: usize,
}

impl PlateLayout {
    /// Return no plate when padding and margins leave fewer than two cells in
    /// either axis. Creating or changing a layout is an infrequent resize
    /// operation.
    pub fn new(
        terminal_width: u16,
        terminal_height: u16,
        config: &LayoutConfig,
        cell_aspect: f32,
        glyphs: GlyphSet,
        margins: Margins,
    ) -> Option<Self> {
        if !cell_aspect.is_finite() || cell_aspect <= 0.0 {
            return None;
        }
        let [vertical, horizontal] = config.padding;
        let available_width = terminal_width
            .saturating_sub(horizontal.saturating_mul(2))
            .saturating_sub(margins.left.saturating_add(margins.right));
        let available_height = terminal_height
            .saturating_sub(vertical.saturating_mul(2))
            .saturating_sub(margins.top.saturating_add(margins.bottom));
        if available_width < 2 || available_height < 2 {
            return None;
        }
        let (width, height) = match config.fit {
            Fit::Stretch => (available_width, available_height),
            Fit::Square => {
                let side =
                    (f32::from(available_width) * cell_aspect).min(f32::from(available_height));
                (
                    (side / cell_aspect).floor().min(f32::from(available_width)) as u16,
                    side.floor().min(f32::from(available_height)) as u16,
                )
            }
        };
        if width < 2 || height < 2 {
            return None;
        }
        let spare_x = available_width - width;
        let spare_y = available_height - height;
        let x = horizontal
            + margins.left
            + match config.align {
                Alignment::Left => 0,
                Alignment::Right => spare_x,
                _ => spare_x / 2,
            };
        let y = vertical
            + margins.top
            + match config.align {
                Alignment::Top => 0,
                Alignment::Bottom => spare_y,
                _ => spare_y / 2,
            };
        let (dots_x, dots_y) = glyphs.subpixels();
        Some(Self {
            x,
            y,
            width,
            height,
            subpixel_width: usize::from(width) * dots_x,
            subpixel_height: usize::from(height) * dots_y,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_is_physically_square_and_centered() {
        let layout = PlateLayout::new(
            80,
            40,
            &LayoutConfig::default(),
            0.5,
            GlyphSet::Braille,
            Margins::default(),
        )
        .unwrap();
        assert_eq!(
            (layout.x, layout.y, layout.width, layout.height),
            (2, 1, 76, 38)
        );
        assert_eq!((layout.subpixel_width, layout.subpixel_height), (152, 152));
    }

    #[test]
    fn wide_and_tall_terminals_keep_the_same_physical_aspect() {
        let wide = PlateLayout::new(
            120,
            30,
            &LayoutConfig::default(),
            0.5,
            GlyphSet::Braille,
            Margins::default(),
        )
        .unwrap();
        assert_eq!((wide.x, wide.y, wide.width, wide.height), (32, 1, 56, 28));
        let tall = PlateLayout::new(
            40,
            80,
            &LayoutConfig::default(),
            0.5,
            GlyphSet::Braille,
            Margins::default(),
        )
        .unwrap();
        assert_eq!((tall.x, tall.y, tall.width, tall.height), (2, 31, 36, 18));
    }

    #[test]
    fn stretch_and_alignment_are_respected() {
        let mut config = LayoutConfig {
            fit: Fit::Stretch,
            ..LayoutConfig::default()
        };
        let stretched =
            PlateLayout::new(120, 30, &config, 0.5, GlyphSet::Ascii, Margins::default()).unwrap();
        assert_eq!((stretched.width, stretched.height), (116, 28));
        assert_eq!(
            (stretched.subpixel_width, stretched.subpixel_height),
            (116, 28)
        );
        config.fit = Fit::Square;
        config.align = Alignment::Right;
        let right =
            PlateLayout::new(120, 30, &config, 0.5, GlyphSet::Braille, Margins::default()).unwrap();
        assert_eq!(right.x, 62);
        config.align = Alignment::Bottom;
        let bottom =
            PlateLayout::new(40, 80, &config, 0.5, GlyphSet::Braille, Margins::default()).unwrap();
        assert_eq!(bottom.y, 61);
    }

    #[test]
    fn tiny_terminals_and_invalid_aspects_do_not_underflow() {
        for (width, height) in [(0, 0), (1, 1), (4, 2), (5, 3), (80, 1)] {
            assert!(
                PlateLayout::new(
                    width,
                    height,
                    &LayoutConfig::default(),
                    0.5,
                    GlyphSet::Braille,
                    Margins::default()
                )
                .is_none()
            );
        }
        assert!(
            PlateLayout::new(
                80,
                40,
                &LayoutConfig::default(),
                f32::NAN,
                GlyphSet::Braille,
                Margins::default()
            )
            .is_none()
        );
    }
}
