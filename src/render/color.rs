//! Terminal color capabilities, OKLab interpolation, and the conversion of
//! configured colors to what a given terminal can show.

use std::{env, io::Write};

use crossterm::style::Color as TermColor;

use crate::config::{color::Color, schema::ColorMode};

/// How many colors the output can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorLevel {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

/// The slice of the process environment that decides what the terminal gets.
/// Reading it once, here, keeps everything else pure and testable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// The effective locale: `LC_ALL`, then `LC_CTYPE`, then `LANG`.
    pub locale: Option<String>,
    pub term: Option<String>,
    pub colorterm: Option<String>,
    /// `NO_COLOR` is set and not empty.
    pub no_color: bool,
}

impl Environment {
    pub fn from_process() -> Self {
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .filter_map(|name| env::var(name).ok())
            .find(|value| !value.is_empty());
        Self {
            locale,
            term: env::var("TERM").ok(),
            colorterm: env::var("COLORTERM").ok(),
            no_color: env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()),
        }
    }
}

impl Environment {
    /// Whether the terminal has an alternate screen to draw on. The Linux
    /// console does not: it ignores the request, so the picture would stay on
    /// the screen after the program ends.
    pub fn has_alternate_screen(&self) -> bool {
        !matches!(self.term.as_deref(), Some("linux"))
    }
}

impl ColorLevel {
    /// Resolve `render.color_mode`. An explicit mode always wins; `auto` honors
    /// `NO_COLOR`, then picks truecolor for `COLORTERM=truecolor|24bit`, 16
    /// colors for the Linux console, no color for a dumb terminal, and 256
    /// colors for everything else.
    pub fn resolve(mode: ColorMode, environment: &Environment) -> Self {
        match mode {
            ColorMode::None => Self::None,
            ColorMode::Ansi16 => Self::Ansi16,
            ColorMode::Ansi256 => Self::Ansi256,
            ColorMode::Truecolor => Self::TrueColor,
            ColorMode::Auto => {
                if environment.no_color {
                    Self::None
                } else if matches!(
                    environment.colorterm.as_deref(),
                    Some("truecolor" | "24bit")
                ) {
                    Self::TrueColor
                } else {
                    match environment.term.as_deref() {
                        Some("linux") => Self::Ansi16,
                        Some("dumb") => Self::None,
                        _ => Self::Ansi256,
                    }
                }
            }
        }
    }
}

/// A color in the OKLab perceptual space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

fn to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn from_linear(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

impl Lab {
    /// Björn Ottosson's OKLab, from sRGB.
    pub fn from_rgb([r, g, b]: [u8; 3]) -> Self {
        let (r, g, b) = (to_linear(r), to_linear(g), to_linear(b));
        let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
        let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
        let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
        Self {
            l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
            a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
            b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
        }
    }

    pub fn to_rgb(self) -> [u8; 3] {
        let l = (self.l + 0.396_337_78 * self.a + 0.215_803_76 * self.b).powi(3);
        let m = (self.l - 0.105_561_346 * self.a - 0.063_854_17 * self.b).powi(3);
        let s = (self.l - 0.089_484_18 * self.a - 1.291_485_5 * self.b).powi(3);
        [
            from_linear(4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s),
            from_linear(-1.268_438 * l + 2.609_757_4 * m - 0.341_319_4 * s),
            from_linear(-0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s),
        ]
    }

    pub fn mix(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            l: self.l + (other.l - self.l) * t,
            a: self.a + (other.a - self.a) * t,
            b: self.b + (other.b - self.b) * t,
        }
    }

    fn distance_squared(self, other: Self) -> f32 {
        (self.l - other.l).powi(2) + (self.a - other.a).powi(2) + (self.b - other.b).powi(2)
    }
}

/// Interpolate two sRGB colors in OKLab.
pub fn mix_rgb(from: [u8; 3], to: [u8; 3], t: f32) -> [u8; 3] {
    Lab::from_rgb(from).mix(Lab::from_rgb(to), t).to_rgb()
}

/// xterm's default RGB values for the sixteen ANSI colors. A terminal's own
/// theme may differ; these only guide the choice of the nearest one.
const ANSI16_RGB: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0xcd, 0x00, 0x00],
    [0x00, 0xcd, 0x00],
    [0xcd, 0xcd, 0x00],
    [0x00, 0x00, 0xee],
    [0xcd, 0x00, 0xcd],
    [0x00, 0xcd, 0xcd],
    [0xe5, 0xe5, 0xe5],
    [0x7f, 0x7f, 0x7f],
    [0xff, 0x00, 0x00],
    [0x00, 0xff, 0x00],
    [0xff, 0xff, 0x00],
    [0x5c, 0x5c, 0xff],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0xff],
    [0xff, 0xff, 0xff],
];

/// The levels of the 6x6x6 color cube of the xterm 256-color palette.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The RGB value of an entry of the xterm 256-color palette.
pub fn xterm_rgb(index: u8) -> [u8; 3] {
    match index {
        0..=15 => ANSI16_RGB[usize::from(index)],
        16..=231 => {
            let cube = usize::from(index - 16);
            [
                CUBE_LEVELS[cube / 36],
                CUBE_LEVELS[cube / 6 % 6],
                CUBE_LEVELS[cube % 6],
            ]
        }
        232..=255 => {
            let level = 8 + 10 * (index - 232);
            [level, level, level]
        }
    }
}

fn nearest(rgb: [u8; 3], candidates: impl Iterator<Item = u8>) -> u8 {
    let target = Lab::from_rgb(rgb);
    candidates
        .map(|index| {
            (
                index,
                Lab::from_rgb(xterm_rgb(index)).distance_squared(target),
            )
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(0, |(index, _)| index)
}

/// The closest of the sixteen ANSI colors.
pub fn nearest_ansi16(rgb: [u8; 3]) -> u8 {
    nearest(rgb, 0..16)
}

/// The closest entry of the 6x6x6 cube or the grayscale ramp. The first
/// sixteen entries are left out: they depend on the terminal's theme.
pub fn nearest_ansi256(rgb: [u8; 3]) -> u8 {
    nearest(rgb, 16..=255)
}

/// What a configured color looks like on a terminal of the given level.
/// `None` means "the terminal's own color" (and, at level `None`, no color).
pub fn to_term(color: Color, level: ColorLevel) -> Option<TermColor> {
    if level == ColorLevel::None {
        return None;
    }
    match color {
        Color::Default => None,
        Color::Ansi(ansi) => Some(TermColor::AnsiValue(ansi.index())),
        Color::Indexed(index) if level == ColorLevel::Ansi16 && index >= 16 => {
            Some(TermColor::AnsiValue(nearest_ansi16(xterm_rgb(index))))
        }
        Color::Indexed(index) => Some(TermColor::AnsiValue(index)),
        Color::Rgb { r, g, b } => Some(match level {
            ColorLevel::TrueColor => TermColor::Rgb { r, g, b },
            ColorLevel::Ansi256 => TermColor::AnsiValue(nearest_ansi256([r, g, b])),
            _ => TermColor::AnsiValue(nearest_ansi16([r, g, b])),
        }),
    }
}

/// Interpolate two configured colors. Only explicit RGB colors can be blended,
/// because the palette a terminal gives an ANSI color is not known; anything
/// else switches halfway through.
pub fn mix_colors(from: Color, to: Color, t: f32) -> Color {
    match (from, to) {
        (
            Color::Rgb { r, g, b },
            Color::Rgb {
                r: r2,
                g: g2,
                b: b2,
            },
        ) => {
            let [r, g, b] = mix_rgb([r, g, b], [r2, g2, b2], t);
            Color::Rgb { r, g, b }
        }
        _ if t < 0.5 => from,
        _ => to,
    }
}

/// The index of an ANSI-palette color, if a terminal color is one.
fn ansi_index(color: TermColor) -> Option<u8> {
    Some(match color {
        TermColor::Black => 0,
        TermColor::DarkRed => 1,
        TermColor::DarkGreen => 2,
        TermColor::DarkYellow => 3,
        TermColor::DarkBlue => 4,
        TermColor::DarkMagenta => 5,
        TermColor::DarkCyan => 6,
        TermColor::Grey => 7,
        TermColor::DarkGrey => 8,
        TermColor::Red => 9,
        TermColor::Green => 10,
        TermColor::Yellow => 11,
        TermColor::Blue => 12,
        TermColor::Magenta => 13,
        TermColor::Cyan => 14,
        TermColor::White => 15,
        TermColor::AnsiValue(index) => index,
        TermColor::Reset | TermColor::Rgb { .. } => return None,
    })
}

/// Write the SGR sequence that sets the foreground or background to `color`;
/// `None` restores the terminal's own. The first sixteen palette entries use
/// the classic `30`-`37` and `90`-`97` codes, which even the Linux console
/// understands.
pub fn write_sgr(
    out: &mut impl Write,
    color: Option<TermColor>,
    foreground: bool,
) -> std::io::Result<()> {
    let base = if foreground { 30 } else { 40 };
    match color {
        None | Some(TermColor::Reset) => write!(out, "\x1b[{}m", base + 9),
        Some(TermColor::Rgb { r, g, b }) => write!(out, "\x1b[{};2;{r};{g};{b}m", base + 8),
        Some(color) => match ansi_index(color) {
            Some(index @ 0..=7) => write!(out, "\x1b[{}m", base + u32::from(index)),
            Some(index @ 8..=15) => write!(out, "\x1b[{}m", base + 60 + u32::from(index - 8)),
            Some(index) => write!(out, "\x1b[{};5;{index}m", base + 8),
            None => Ok(()),
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::color::AnsiColor;

    fn environment(term: Option<&str>, colorterm: Option<&str>, no_color: bool) -> Environment {
        Environment {
            locale: None,
            term: term.map(str::to_owned),
            colorterm: colorterm.map(str::to_owned),
            no_color,
        }
    }

    #[test]
    fn only_the_linux_console_lacks_an_alternate_screen() {
        let has = |term: Option<&str>| environment(term, None, false).has_alternate_screen();
        assert!(!has(Some("linux")));
        for term in [
            "xterm-256color",
            "tmux-256color",
            "screen",
            "alacritty",
            "dumb",
        ] {
            assert!(has(Some(term)), "{term}");
        }
        assert!(has(None));
    }

    #[test]
    fn auto_color_follows_colorterm_term_and_no_color() {
        let level = |term, colorterm, no_color| {
            ColorLevel::resolve(ColorMode::Auto, &environment(term, colorterm, no_color))
        };
        assert_eq!(
            level(Some("xterm-256color"), Some("truecolor"), false),
            ColorLevel::TrueColor
        );
        assert_eq!(
            level(Some("xterm"), Some("24bit"), false),
            ColorLevel::TrueColor
        );
        assert_eq!(
            level(Some("xterm-256color"), None, false),
            ColorLevel::Ansi256
        );
        assert_eq!(
            level(Some("xterm-256color"), Some("yes"), false),
            ColorLevel::Ansi256
        );
        assert_eq!(level(None, None, false), ColorLevel::Ansi256);
        assert_eq!(level(Some("linux"), None, false), ColorLevel::Ansi16);
        assert_eq!(
            level(Some("linux"), Some("truecolor"), false),
            ColorLevel::TrueColor
        );
        assert_eq!(level(Some("dumb"), None, false), ColorLevel::None);
        // NO_COLOR beats every automatic guess ...
        assert_eq!(
            level(Some("xterm"), Some("truecolor"), true),
            ColorLevel::None
        );
        // ... but not an explicit choice.
        let forced = environment(Some("xterm"), None, true);
        assert_eq!(
            ColorLevel::resolve(ColorMode::Truecolor, &forced),
            ColorLevel::TrueColor
        );
        assert_eq!(
            ColorLevel::resolve(ColorMode::Ansi256, &forced),
            ColorLevel::Ansi256
        );
        assert_eq!(
            ColorLevel::resolve(ColorMode::Ansi16, &forced),
            ColorLevel::Ansi16
        );
        assert_eq!(
            ColorLevel::resolve(
                ColorMode::None,
                &environment(None, Some("truecolor"), false)
            ),
            ColorLevel::None
        );
    }

    #[test]
    fn oklab_round_trips_and_orders_lightness() {
        for rgb in [
            [0, 0, 0],
            [255, 255, 255],
            [232, 213, 176],
            [10, 200, 90],
            [128, 128, 128],
        ] {
            let back = Lab::from_rgb(rgb).to_rgb();
            for channel in 0..3 {
                assert!(
                    (i16::from(back[channel]) - i16::from(rgb[channel])).abs() <= 1,
                    "{rgb:?} -> {back:?}"
                );
            }
        }
        let white = Lab::from_rgb([255, 255, 255]);
        assert!((white.l - 1.0).abs() < 1e-3 && white.a.abs() < 1e-3 && white.b.abs() < 1e-3);
        assert!(Lab::from_rgb([0, 0, 0]).l.abs() < 1e-3);
        // The perceptual midpoint (OKLab L = 0.5) is darker than sRGB 50 %.
        let [mid, ..] = mix_rgb([0, 0, 0], [255, 255, 255], 0.5);
        assert!((97..=101).contains(&mid), "{mid}");
        assert_eq!(mix_rgb([10, 20, 30], [200, 100, 50], 0.0), [10, 20, 30]);
        assert_eq!(mix_rgb([10, 20, 30], [200, 100, 50], 1.0), [200, 100, 50]);
    }

    #[test]
    fn xterm_palette_values_are_standard() {
        assert_eq!(xterm_rgb(0), [0, 0, 0]);
        assert_eq!(xterm_rgb(15), [255, 255, 255]);
        assert_eq!(xterm_rgb(16), [0, 0, 0]);
        assert_eq!(xterm_rgb(21), [0, 0, 255]);
        assert_eq!(xterm_rgb(196), [255, 0, 0]);
        assert_eq!(xterm_rgb(231), [255, 255, 255]);
        assert_eq!(xterm_rgb(232), [8, 8, 8]);
        assert_eq!(xterm_rgb(255), [238, 238, 238]);
    }

    #[test]
    fn rgb_is_quantized_to_the_nearest_palette_entry() {
        assert_eq!(nearest_ansi16([255, 0, 0]), 9);
        assert_eq!(nearest_ansi16([0, 0, 0]), 0);
        assert_eq!(nearest_ansi16([250, 250, 250]), 15);
        assert_eq!(nearest_ansi16([120, 120, 120]), 8);
        assert_eq!(nearest_ansi256([255, 0, 0]), 196);
        assert_eq!(nearest_ansi256([0, 0, 255]), 21);
        assert_eq!(nearest_ansi256([238, 238, 238]), 255);
        // A mid-gray lands on the grayscale ramp, never on a theme-dependent entry.
        let gray = nearest_ansi256([100, 100, 100]);
        assert!(
            (232..=255).contains(&gray) || (16..=231).contains(&gray),
            "{gray}"
        );
        assert!(gray >= 16);
        // Quantizing a palette color's own value finds it again.
        for index in (16..=255).step_by(7) {
            assert_eq!(nearest_ansi256(xterm_rgb(index)), index);
        }
    }

    #[test]
    fn configured_colors_follow_the_color_level() {
        let red = Color::Rgb { r: 255, g: 0, b: 0 };
        assert_eq!(
            to_term(red, ColorLevel::TrueColor),
            Some(TermColor::Rgb { r: 255, g: 0, b: 0 })
        );
        assert_eq!(
            to_term(red, ColorLevel::Ansi256),
            Some(TermColor::AnsiValue(196))
        );
        assert_eq!(
            to_term(red, ColorLevel::Ansi16),
            Some(TermColor::AnsiValue(9))
        );
        assert_eq!(to_term(red, ColorLevel::None), None);
        // Palette colors follow the terminal's theme and are never converted.
        let blue = Color::Ansi(AnsiColor::Blue);
        for level in [
            ColorLevel::Ansi16,
            ColorLevel::Ansi256,
            ColorLevel::TrueColor,
        ] {
            assert_eq!(to_term(blue, level), Some(TermColor::AnsiValue(4)));
            assert_eq!(to_term(Color::Default, level), None);
        }
        assert_eq!(to_term(blue, ColorLevel::None), None);
        assert_eq!(
            to_term(Color::Indexed(200), ColorLevel::Ansi256),
            Some(TermColor::AnsiValue(200))
        );
        // A 256-color index shrinks to the nearest of the sixteen.
        assert_eq!(
            to_term(Color::Indexed(196), ColorLevel::Ansi16),
            Some(TermColor::AnsiValue(9))
        );
        assert_eq!(
            to_term(Color::Indexed(7), ColorLevel::Ansi16),
            Some(TermColor::AnsiValue(7))
        );
    }

    #[test]
    fn only_rgb_colors_blend_and_palette_colors_switch_halfway() {
        let a = Color::Rgb { r: 0, g: 0, b: 0 };
        let b = Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        assert_eq!(mix_colors(a, b, 0.0), a);
        assert_eq!(mix_colors(a, b, 1.0), b);
        assert!(matches!(mix_colors(a, b, 0.5), Color::Rgb { r, .. } if (97..=101).contains(&r)));
        let red = Color::Ansi(AnsiColor::Red);
        let green = Color::Ansi(AnsiColor::Green);
        assert_eq!(mix_colors(red, green, 0.49), red);
        assert_eq!(mix_colors(red, green, 0.5), green);
        assert_eq!(mix_colors(a, red, 0.2), a);
    }

    fn sgr(color: Option<TermColor>, foreground: bool) -> String {
        let mut bytes = Vec::new();
        write_sgr(&mut bytes, color, foreground).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn sgr_uses_the_shortest_code_every_terminal_understands() {
        assert_eq!(sgr(Some(TermColor::AnsiValue(1)), true), "\x1b[31m");
        assert_eq!(sgr(Some(TermColor::AnsiValue(7)), false), "\x1b[47m");
        assert_eq!(sgr(Some(TermColor::AnsiValue(8)), true), "\x1b[90m");
        assert_eq!(sgr(Some(TermColor::AnsiValue(15)), false), "\x1b[107m");
        assert_eq!(sgr(Some(TermColor::AnsiValue(16)), true), "\x1b[38;5;16m");
        assert_eq!(
            sgr(Some(TermColor::AnsiValue(255)), false),
            "\x1b[48;5;255m"
        );
        assert_eq!(
            sgr(Some(TermColor::Rgb { r: 1, g: 2, b: 3 }), true),
            "\x1b[38;2;1;2;3m"
        );
        assert_eq!(
            sgr(Some(TermColor::Rgb { r: 1, g: 2, b: 3 }), false),
            "\x1b[48;2;1;2;3m"
        );
        assert_eq!(sgr(None, true), "\x1b[39m");
        assert_eq!(sgr(None, false), "\x1b[49m");
        // crossterm's named colors are the same palette entries.
        assert_eq!(sgr(Some(TermColor::Red), true), "\x1b[91m");
        assert_eq!(sgr(Some(TermColor::DarkRed), true), "\x1b[31m");
        assert_eq!(sgr(Some(TermColor::Reset), false), "\x1b[49m");
    }
}
