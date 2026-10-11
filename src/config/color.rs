//! Terminal palette and RGB colors accepted by configuration files.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// An ANSI color whose appearance follows the terminal's own palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnsiColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

impl AnsiColor {
    /// The corresponding index in the terminal's first sixteen palette entries.
    pub const fn index(self) -> u8 {
        match self {
            Self::Black => 0,
            Self::Red => 1,
            Self::Green => 2,
            Self::Yellow => 3,
            Self::Blue => 4,
            Self::Magenta => 5,
            Self::Cyan => 6,
            Self::White => 7,
            Self::BrightBlack => 8,
            Self::BrightRed => 9,
            Self::BrightGreen => 10,
            Self::BrightYellow => 11,
            Self::BrightBlue => 12,
            Self::BrightMagenta => 13,
            Self::BrightCyan => 14,
            Self::BrightWhite => 15,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Black => "black",
            Self::Red => "red",
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Blue => "blue",
            Self::Magenta => "magenta",
            Self::Cyan => "cyan",
            Self::White => "white",
            Self::BrightBlack => "bright-black",
            Self::BrightRed => "bright-red",
            Self::BrightGreen => "bright-green",
            Self::BrightYellow => "bright-yellow",
            Self::BrightBlue => "bright-blue",
            Self::BrightMagenta => "bright-magenta",
            Self::BrightCyan => "bright-cyan",
            Self::BrightWhite => "bright-white",
        }
    }
}

/// A terminal default, ANSI name, indexed color, or explicit RGB color.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Ansi(AnsiColor),
    Indexed(u8),
    Rgb {
        r: u8,
        g: u8,
        b: u8,
    },
}

/// Explains the supported syntax when a color cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid color {value:?}: expected default, an ANSI name (black..white or bright-black..bright-white), a palette index 0..255, or #rrggbb"
)]
pub struct ParseColorError {
    value: String,
}

impl FromStr for Color {
    type Err = ParseColorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let ansi = match value {
            "default" => return Ok(Self::Default),
            "black" => Some(AnsiColor::Black),
            "red" => Some(AnsiColor::Red),
            "green" => Some(AnsiColor::Green),
            "yellow" => Some(AnsiColor::Yellow),
            "blue" => Some(AnsiColor::Blue),
            "magenta" => Some(AnsiColor::Magenta),
            "cyan" => Some(AnsiColor::Cyan),
            "white" => Some(AnsiColor::White),
            "bright-black" => Some(AnsiColor::BrightBlack),
            "bright-red" => Some(AnsiColor::BrightRed),
            "bright-green" => Some(AnsiColor::BrightGreen),
            "bright-yellow" => Some(AnsiColor::BrightYellow),
            "bright-blue" => Some(AnsiColor::BrightBlue),
            "bright-magenta" => Some(AnsiColor::BrightMagenta),
            "bright-cyan" => Some(AnsiColor::BrightCyan),
            "bright-white" => Some(AnsiColor::BrightWhite),
            _ => None,
        };
        if let Some(color) = ansi {
            return Ok(Self::Ansi(color));
        }
        let index = (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| value.parse::<u8>().ok())
            .flatten();
        if let Some(index) = index {
            return Ok(Self::Indexed(index));
        }
        let rgb = value
            .strip_prefix('#')
            .filter(|hex| hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .and_then(|hex| u32::from_str_radix(hex, 16).ok());
        if let Some(rgb) = rgb {
            return Ok(Self::Rgb {
                r: ((rgb >> 16) & 0xff) as u8,
                g: ((rgb >> 8) & 0xff) as u8,
                b: (rgb & 0xff) as u8,
            });
        }
        Err(ParseColorError {
            value: value.to_owned(),
        })
    }
}

impl fmt::Display for Color {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => formatter.write_str("default"),
            Self::Ansi(color) => formatter.write_str(color.name()),
            Self::Indexed(index) => write!(formatter, "{index}"),
            Self::Rgb { r, g, b } => write!(formatter, "#{r:02x}{g:02x}{b:02x}"),
        }
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{AnsiColor, Color};

    #[test]
    fn accepts_all_ansi_names_and_palette_indices() {
        let names = [
            "black",
            "red",
            "green",
            "yellow",
            "blue",
            "magenta",
            "cyan",
            "white",
            "bright-black",
            "bright-red",
            "bright-green",
            "bright-yellow",
            "bright-blue",
            "bright-magenta",
            "bright-cyan",
            "bright-white",
        ];
        for (index, name) in names.iter().enumerate() {
            let color = name.parse::<Color>().unwrap();
            let Color::Ansi(ansi) = color else {
                panic!("ANSI name parsed into a different color kind");
            };
            assert_eq!(ansi.index(), index as u8);
            assert_eq!(color.to_string(), *name);
        }
        for index in 0..=255_u8 {
            assert_eq!(
                index.to_string().parse::<Color>().unwrap(),
                Color::Indexed(index)
            );
        }
        assert_eq!("default".parse::<Color>().unwrap(), Color::Default);
        assert_eq!(
            "bright-cyan".parse::<Color>().unwrap(),
            Color::Ansi(AnsiColor::BrightCyan)
        );
    }

    #[test]
    fn accepts_rgb_case_and_serializes_canonical_colors() {
        let color = "#aB01Ff".parse::<Color>().unwrap();
        assert_eq!(
            color,
            Color::Rgb {
                r: 171,
                g: 1,
                b: 255
            }
        );
        assert_eq!(color.to_string(), "#ab01ff");
        for color in [
            Color::Default,
            Color::Ansi(AnsiColor::Red),
            Color::Indexed(255),
            color,
        ] {
            let encoded = serde_json::to_string(&color).unwrap();
            assert_eq!(serde_json::from_str::<Color>(&encoded).unwrap(), color);
        }
    }

    #[test]
    fn rejects_invalid_or_ambiguous_colors_with_helpful_errors() {
        for value in [
            "", "256", "-1", "+1", " 1", "red ", "RED", "#fff", "#gg0000", "#1234567", "blue\n",
        ] {
            let error = value.parse::<Color>().unwrap_err().to_string();
            assert!(error.contains("0..255"), "{error}");
            assert!(error.contains("#rrggbb"), "{error}");
        }
        assert!(serde_json::from_str::<Color>("7").is_err());
    }
}
