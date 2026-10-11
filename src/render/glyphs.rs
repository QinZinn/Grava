//! Character encodings for sub-pixel particle occupancy.

use std::env;

use crate::config::schema::Glyphs;

/// Braille bits indexed by row, then column.
pub const BRAILLE_BITS: [[u8; 2]; 4] = [[1, 8], [2, 16], [4, 32], [64, 128]];

/// Quadrant bits are top-left, top-right, bottom-left, bottom-right.
pub const QUADRANT_GLYPHS: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

/// Half-block bits are top, bottom.
pub const HALF_GLYPHS: [char; 4] = [' ', '▀', '▄', '█'];

/// A character set that turns a grid of sub-pixels into one terminal cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphSet {
    /// 2x4 sub-pixels per cell.
    Braille,
    /// 2x2 sub-pixels per cell.
    Quadrant,
    /// 1x2 sub-pixels per cell.
    Half,
    /// One sub-pixel per cell, drawn from a density ramp.
    Ascii,
}

impl GlyphSet {
    /// Select auto glyphs from the effective locale, preserving explicit choices.
    /// `auto` is Braille on a UTF-8 locale unless `TERM` is the Linux console,
    /// whose font has no Braille; everything else gets plain ASCII.
    pub fn resolve(requested: Glyphs, locale: Option<&str>, term: Option<&str>) -> Self {
        match requested {
            Glyphs::Braille => Self::Braille,
            Glyphs::Quadrant => Self::Quadrant,
            Glyphs::Half => Self::Half,
            Glyphs::Ascii => Self::Ascii,
            Glyphs::Auto => {
                let utf8 = locale.is_some_and(|locale| {
                    let locale = locale.to_ascii_lowercase();
                    locale.contains("utf-8") || locale.contains("utf8")
                });
                if utf8 && term != Some("linux") {
                    Self::Braille
                } else {
                    Self::Ascii
                }
            }
        }
    }

    /// POSIX locale precedence is LC_ALL, then LC_CTYPE, then LANG.
    pub fn from_environment(requested: Glyphs) -> Self {
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .filter_map(|name| env::var(name).ok())
            .find(|value| !value.is_empty());
        let term = env::var("TERM").ok();
        Self::resolve(requested, locale.as_deref(), term.as_deref())
    }

    /// Sub-pixel columns and rows in one terminal cell.
    pub const fn subpixels(self) -> (usize, usize) {
        match self {
            Self::Braille => (2, 4),
            Self::Quadrant => (2, 2),
            Self::Half => (1, 2),
            Self::Ascii => (1, 1),
        }
    }

    /// The configuration spelling, for the HUD and messages.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Braille => "braille",
            Self::Quadrant => "quadrant",
            Self::Half => "half",
            Self::Ascii => "ascii",
        }
    }

    /// The glyph set a configuration value spells explicitly.
    pub const fn config_value(self) -> Glyphs {
        match self {
            Self::Braille => Glyphs::Braille,
            Self::Quadrant => Glyphs::Quadrant,
            Self::Half => Glyphs::Half,
            Self::Ascii => Glyphs::Ascii,
        }
    }

    /// Whether the characters need a Unicode-capable terminal font.
    pub const fn is_unicode(self) -> bool {
        !matches!(self, Self::Ascii)
    }

    /// The next set in the order of the `g` key.
    pub const fn next(self) -> Self {
        match self {
            Self::Braille => Self::Quadrant,
            Self::Quadrant => Self::Half,
            Self::Half => Self::Ascii,
            Self::Ascii => Self::Braille,
        }
    }

    /// The bit of the sub-pixel at `(column, row)` in a cell. ASCII has no bit
    /// layout: its single sub-pixel is read as a density.
    pub const fn bit(self, column: usize, row: usize) -> u8 {
        match self {
            Self::Braille => BRAILLE_BITS[row][column],
            Self::Quadrant => 1 << (row * 2 + column),
            Self::Half => 1 << row,
            Self::Ascii => 1,
        }
    }

    /// The character for a cell whose lit sub-pixels are `bits`; an empty
    /// cell is a plain space, which also keeps copied text clean. Not for ASCII,
    /// which maps a density to a character instead.
    pub fn glyph(self, bits: u8) -> char {
        match self {
            Self::Braille if bits == 0 => ' ',
            Self::Braille => braille(bits),
            Self::Quadrant => QUADRANT_GLYPHS[usize::from(bits & 0x0f)],
            Self::Half => HALF_GLYPHS[usize::from(bits & 0x03)],
            Self::Ascii => {
                if bits == 0 {
                    ' '
                } else {
                    '#'
                }
            }
        }
    }
}

/// Every value in U+2800..U+28FF is a valid Unicode scalar.
pub fn braille(bits: u8) -> char {
    char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' ')
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn braille_bits_match_unicode_positions() {
        let expected = [['⠁', '⠈'], ['⠂', '⠐'], ['⠄', '⠠'], ['⡀', '⢀']];
        for (row, expected_row) in BRAILLE_BITS.iter().zip(expected) {
            for (bits, character) in row.iter().zip(expected_row) {
                assert_eq!(braille(*bits), character);
            }
        }
        assert_eq!(braille(0), '⠀');
        assert_eq!(braille(255), '⣿');
    }

    #[test]
    fn quadrant_table_has_the_documented_bit_order() {
        assert_eq!(
            QUADRANT_GLYPHS.iter().collect::<String>(),
            " ▘▝▀▖▌▞▛▗▚▐▜▄▙▟█"
        );
        assert_eq!(QUADRANT_GLYPHS[1 | 2], '▀');
        assert_eq!(QUADRANT_GLYPHS[1 | 4], '▌');
        assert_eq!(QUADRANT_GLYPHS[2 | 8], '▐');
        assert_eq!(QUADRANT_GLYPHS[4 | 8], '▄');
        assert_eq!(HALF_GLYPHS.iter().collect::<String>(), " ▀▄█");
    }

    #[test]
    fn sub_pixel_bits_pick_the_matching_glyph_in_every_set() {
        // Quadrants: top-left, top-right, bottom-left, bottom-right.
        let quadrant = |column, row| GlyphSet::Quadrant.glyph(GlyphSet::Quadrant.bit(column, row));
        assert_eq!(
            [
                quadrant(0, 0),
                quadrant(1, 0),
                quadrant(0, 1),
                quadrant(1, 1)
            ],
            ['▘', '▝', '▖', '▗']
        );
        // Halves: top, bottom.
        let half = |row| GlyphSet::Half.glyph(GlyphSet::Half.bit(0, row));
        assert_eq!([half(0), half(1)], ['▀', '▄']);
        // Braille follows the documented bit table cell by cell.
        let braille_bit = |column, row| GlyphSet::Braille.glyph(GlyphSet::Braille.bit(column, row));
        assert_eq!(braille_bit(0, 0), '⠁');
        assert_eq!(braille_bit(1, 3), '⢀');
        for set in [GlyphSet::Braille, GlyphSet::Quadrant, GlyphSet::Half] {
            let (columns, rows) = set.subpixels();
            let mut all = 0;
            for row in 0..rows {
                for column in 0..columns {
                    let bit = set.bit(column, row);
                    assert_eq!(bit.count_ones(), 1, "{set:?}");
                    assert_eq!(all & bit, 0, "{set:?} reuses a bit");
                    all |= bit;
                }
            }
            assert_eq!(all.count_ones() as usize, columns * rows);
            assert_eq!(set.glyph(0), ' ');
        }
        assert_eq!(GlyphSet::Quadrant.glyph(0x0f), '█');
        assert_eq!(GlyphSet::Half.glyph(0x03), '█');
        assert_eq!(GlyphSet::Braille.glyph(0xff), '⣿');
    }

    #[test]
    fn automatic_glyphs_respect_locale_and_linux_console() {
        assert_eq!(
            GlyphSet::resolve(Glyphs::Auto, Some("en_US.UTF-8"), Some("xterm-256color")),
            GlyphSet::Braille
        );
        assert_eq!(
            GlyphSet::resolve(Glyphs::Auto, Some("C.utf8"), None),
            GlyphSet::Braille
        );
        assert_eq!(
            GlyphSet::resolve(Glyphs::Auto, Some("en_US.UTF-8"), Some("linux")),
            GlyphSet::Ascii
        );
        assert_eq!(
            GlyphSet::resolve(Glyphs::Auto, Some("C"), None),
            GlyphSet::Ascii
        );
        assert_eq!(
            GlyphSet::resolve(Glyphs::Braille, Some("C"), Some("linux")),
            GlyphSet::Braille
        );
        assert_eq!(
            GlyphSet::resolve(Glyphs::Quadrant, None, None),
            GlyphSet::Quadrant
        );
        assert_eq!(GlyphSet::resolve(Glyphs::Half, None, None), GlyphSet::Half);
    }

    #[test]
    fn the_g_key_cycles_through_every_set_and_names_round_trip() {
        let mut set = GlyphSet::Braille;
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(set);
            assert_eq!(GlyphSet::resolve(set.config_value(), None, None), set);
            set = set.next();
        }
        assert_eq!(set, GlyphSet::Braille);
        assert_eq!(seen.len(), 4);
        assert!(
            seen.iter()
                .all(|a| seen.iter().filter(|b| *b == a).count() == 1)
        );
        assert_eq!(
            seen.iter().map(|set| set.name()).collect::<Vec<_>>(),
            ["braille", "quadrant", "half", "ascii"]
        );
        assert!(GlyphSet::Half.is_unicode() && !GlyphSet::Ascii.is_unicode());
    }
}
