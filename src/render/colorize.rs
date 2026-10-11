//! Turning a color scheme into the colors of cells.
//!
//! A [`Palette`] is a scheme converted for one terminal: every color already
//! reduced to what the color level can show, and the gradient precomputed as
//! a short table, so choosing a cell's color costs a lookup per frame.

use crossterm::style::Color as TermColor;

use crate::config::{color::Color, schema::Colorize, theme::ColorScheme};

use super::color::{ColorLevel, mix_colors, mix_rgb, to_term};

/// What a colorizer knows about a non-empty cell of the plate.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CellInput {
    /// 0..1: how heaped the sand is.
    pub density: f32,
    /// Position in the plate, 0..1 from the left and from the top.
    pub x: f32,
    pub y: f32,
    /// Distance from the plate's centre: 0 in the middle, 1 at the rim.
    pub radius: f32,
    /// 0..1: the vibration amplitude of the field here.
    pub amplitude: f32,
}

/// A color scheme ready to paint with.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    level: ColorLevel,
    colorize: Colorize,
    gradient: Vec<Option<TermColor>>,
    sand: Option<TermColor>,
    sand_color: Color,
    note_colors: [Color; 12],
    pub background: Option<TermColor>,
    pub border: Option<TermColor>,
    pub title: Option<TermColor>,
    pub hud_text: Option<TermColor>,
    pub hud_accent: Option<TermColor>,
    pub hud_dim: Option<TermColor>,
    pub hud_error: Option<TermColor>,
}

impl Palette {
    /// `steps` is `render.gradient_steps`: the gradient has that many colors,
    /// so that neighbouring cells share one and fewer escape codes are sent.
    pub fn new(scheme: &ColorScheme, level: ColorLevel, steps: usize) -> Self {
        let convert = |color: Color| to_term(color, level);
        Self {
            level,
            colorize: scheme.colorize,
            gradient: build_gradient(&scheme.gradient, steps, level),
            sand: convert(scheme.sand),
            sand_color: scheme.sand,
            note_colors: scheme.note_colors,
            background: convert(scheme.background),
            border: convert(scheme.border),
            title: convert(scheme.title),
            hud_text: convert(scheme.hud_text),
            hud_accent: convert(scheme.hud_accent),
            hud_dim: convert(scheme.hud_dim),
            hud_error: convert(scheme.hud_error),
        }
    }

    pub fn colorize(&self) -> Colorize {
        self.colorize
    }

    pub fn level(&self) -> ColorLevel {
        self.level
    }

    pub fn sand(&self) -> Option<TermColor> {
        self.sand
    }

    /// The gradient color at `t` in 0..1, quantized to the gradient's steps.
    pub fn gradient_at(&self, t: f32) -> Option<TermColor> {
        let last = self.gradient.len() - 1;
        let t = if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.gradient[(t * last as f32).round() as usize]
    }

    /// The color of a note; while the note changes, `from` fades into `to`
    /// as `progress` goes from 0 to 1. Without a note yet the sand color shows.
    pub fn note(&self, from: Option<u8>, to: Option<u8>, progress: f32) -> Option<TermColor> {
        let raw = |class: Option<u8>| {
            class
                .and_then(|class| self.note_colors.get(usize::from(class)).copied())
                .unwrap_or(self.sand_color)
        };
        to_term(mix_colors(raw(from), raw(to), progress), self.level)
    }

    /// The foreground of one cell. `note` is the (possibly fading) color of
    /// the current note, used by the `note` mode.
    pub fn cell(&self, input: CellInput, note: Option<TermColor>) -> Option<TermColor> {
        match self.colorize {
            Colorize::Solid => self.sand,
            Colorize::Density => self.gradient_at(input.density),
            Colorize::Radial => self.gradient_at(input.radius),
            Colorize::Vertical => self.gradient_at(input.y),
            Colorize::Horizontal => self.gradient_at(input.x),
            // Sand that still dances sits where the plate moves: bright. The
            // square root lifts the gentle motion near the lines out of the dark.
            Colorize::Amplitude => self.gradient_at(input.amplitude.max(0.0).sqrt()),
            Colorize::Note => note,
        }
    }
}

/// The colors of `steps` evenly spaced points along the gradient. Explicit RGB
/// stops blend in OKLab. Palette colors cannot: their appearance belongs to the
/// terminal's theme, so a gradient that has any picks the nearest stop.
fn build_gradient(stops: &[Color], steps: usize, level: ColorLevel) -> Vec<Option<TermColor>> {
    let steps = steps.max(1);
    let rgb: Option<Vec<[u8; 3]>> = stops
        .iter()
        .map(|stop| match stop {
            Color::Rgb { r, g, b } => Some([*r, *g, *b]),
            _ => None,
        })
        .collect();
    let last_stop = stops.len().saturating_sub(1);
    (0..steps)
        .map(|step| {
            // A single step is the middle of the gradient.
            let t = if steps == 1 {
                0.5
            } else {
                step as f32 / (steps - 1) as f32
            };
            match (&rgb, stops.len()) {
                (_, 0) => None,
                (Some(rgb), _) => {
                    let position = t * last_stop as f32;
                    let index = (position.floor() as usize).min(last_stop);
                    let next = (index + 1).min(last_stop);
                    let [r, g, b] = mix_rgb(rgb[index], rgb[next], position - index as f32);
                    to_term(Color::Rgb { r, g, b }, level)
                }
                (None, _) => to_term(stops[(t * last_stop as f32).round() as usize], level),
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::color::AnsiColor;

    fn rgb(hex: &str) -> Color {
        hex.parse().unwrap()
    }

    fn scheme(colorize: Colorize, gradient: &[&str]) -> ColorScheme {
        let mut scheme = ColorScheme::builtin_default();
        scheme.colorize = colorize;
        scheme.gradient = gradient.iter().map(|hex| rgb(hex)).collect();
        scheme
    }

    fn truecolor(hex: &str) -> Option<TermColor> {
        to_term(rgb(hex), ColorLevel::TrueColor)
    }

    #[test]
    fn an_rgb_gradient_runs_through_its_stops_and_blends_between_them() {
        let palette = Palette::new(
            &scheme(Colorize::Density, &["#000000", "#ff0000", "#ffffff"]),
            ColorLevel::TrueColor,
            9,
        );
        assert_eq!(palette.gradient_at(0.0), truecolor("#000000"));
        assert_eq!(palette.gradient_at(0.5), truecolor("#ff0000"));
        assert_eq!(palette.gradient_at(1.0), truecolor("#ffffff"));
        let TermColor::Rgb { r, g, .. } = palette.gradient_at(0.25).unwrap() else {
            panic!("truecolor gradients blend to RGB");
        };
        assert!(
            r > 80 && r < 200 && g < 30,
            "dark red between black and red: {r} {g}"
        );
        // Out-of-range and non-finite inputs clamp.
        assert_eq!(palette.gradient_at(-3.0), palette.gradient_at(0.0));
        assert_eq!(palette.gradient_at(7.0), palette.gradient_at(1.0));
        assert_eq!(palette.gradient_at(f32::NAN), palette.gradient_at(0.0));
    }

    #[test]
    fn the_gradient_is_quantized_to_the_configured_number_of_steps() {
        for steps in [1, 2, 5, 24] {
            let palette = Palette::new(
                &scheme(Colorize::Density, &["#102030", "#a0b0c0"]),
                ColorLevel::TrueColor,
                steps,
            );
            let mut distinct: Vec<_> = (0..=1000)
                .map(|i| palette.gradient_at(i as f32 / 1000.0))
                .collect();
            distinct.dedup();
            assert_eq!(distinct.len(), steps, "{steps} steps");
        }
        let one = Palette::new(
            &scheme(Colorize::Density, &["#000000", "#ffffff"]),
            ColorLevel::TrueColor,
            1,
        );
        assert_eq!(one.gradient_at(0.0), one.gradient_at(1.0));
        // Zero steps (impossible through validation) still yields a color table.
        let zero = Palette::new(
            &scheme(Colorize::Density, &["#000000"]),
            ColorLevel::TrueColor,
            0,
        );
        assert!(zero.gradient_at(0.5).is_some());
    }

    #[test]
    fn palette_gradients_pick_the_nearest_stop_without_inventing_colors() {
        // The default theme: indices 8, 4, 6, 15.
        let palette = Palette::new(&ColorScheme::builtin_default(), ColorLevel::TrueColor, 24);
        let at = |t| palette.gradient_at(t);
        assert_eq!(at(0.0), Some(TermColor::AnsiValue(8)));
        assert_eq!(at(0.33), Some(TermColor::AnsiValue(4)));
        assert_eq!(at(0.66), Some(TermColor::AnsiValue(6)));
        assert_eq!(at(1.0), Some(TermColor::AnsiValue(15)));
        let mut distinct: Vec<_> = (0..=100).map(|i| at(i as f32 / 100.0)).collect();
        distinct.dedup();
        assert_eq!(distinct.len(), 4);
        // A gradient mixing ANSI names and RGB cannot blend either.
        let mut mixed = ColorScheme::builtin_default();
        mixed.gradient = vec![Color::Ansi(AnsiColor::Red), rgb("#00ff00")];
        let palette = Palette::new(&mixed, ColorLevel::TrueColor, 8);
        assert_eq!(palette.gradient_at(0.0), Some(TermColor::AnsiValue(1)));
        assert_eq!(
            palette.gradient_at(1.0),
            Some(TermColor::Rgb { r: 0, g: 255, b: 0 })
        );
    }

    #[test]
    fn colors_follow_the_terminal_level() {
        let scheme = scheme(Colorize::Density, &["#ff0000", "#0000ff"]);
        let for_level = |level| Palette::new(&scheme, level, 4);
        assert_eq!(
            for_level(ColorLevel::TrueColor).gradient_at(0.0),
            truecolor("#ff0000")
        );
        assert_eq!(
            for_level(ColorLevel::Ansi256).gradient_at(0.0),
            Some(TermColor::AnsiValue(196))
        );
        assert_eq!(
            for_level(ColorLevel::Ansi16).gradient_at(0.0),
            Some(TermColor::AnsiValue(9))
        );
        let none = for_level(ColorLevel::None);
        assert_eq!(none.gradient_at(0.5), None);
        assert_eq!(none.sand(), None);
        assert_eq!(none.border, None);
        assert_eq!(none.cell(CellInput::default(), None), None);
        assert_eq!(none.note(Some(0), Some(1), 0.5), None);
    }

    #[test]
    fn each_colorize_mode_reads_its_own_input() {
        let input = |density, x, y, radius, amplitude| CellInput {
            density,
            x,
            y,
            radius,
            amplitude,
        };
        let build = |mode| {
            Palette::new(
                &scheme(mode, &["#000000", "#ffffff"]),
                ColorLevel::TrueColor,
                255,
            )
        };
        let dark = truecolor("#000000");
        let light = truecolor("#ffffff");
        // Density: heaped sand is light.
        let density = build(Colorize::Density);
        assert_eq!(density.cell(input(0.0, 1.0, 1.0, 1.0, 1.0), None), dark);
        assert_eq!(density.cell(input(1.0, 0.0, 0.0, 0.0, 0.0), None), light);
        // Radial, vertical and horizontal ignore the sand and read the position.
        let radial = build(Colorize::Radial);
        assert_eq!(radial.cell(input(1.0, 0.0, 0.0, 0.0, 1.0), None), dark);
        assert_eq!(radial.cell(input(0.0, 0.0, 0.0, 1.0, 0.0), None), light);
        let vertical = build(Colorize::Vertical);
        assert_eq!(vertical.cell(input(1.0, 1.0, 0.0, 1.0, 1.0), None), dark);
        assert_eq!(vertical.cell(input(0.0, 0.0, 1.0, 0.0, 0.0), None), light);
        let horizontal = build(Colorize::Horizontal);
        assert_eq!(horizontal.cell(input(1.0, 0.0, 1.0, 1.0, 1.0), None), dark);
        assert_eq!(horizontal.cell(input(0.0, 1.0, 0.0, 0.0, 0.0), None), light);
        // Amplitude: sand that still moves glows, and gentle motion shows.
        let amplitude = build(Colorize::Amplitude);
        assert_eq!(amplitude.cell(input(1.0, 1.0, 1.0, 1.0, 0.0), None), dark);
        assert_eq!(amplitude.cell(input(0.0, 0.0, 0.0, 0.0, 1.0), None), light);
        let (low, high) = (
            amplitude.cell(input(0.0, 0.0, 0.0, 0.0, 0.04), None),
            amplitude.cell(input(0.0, 0.0, 0.0, 0.0, 0.5), None),
        );
        assert_ne!(low, dark, "square root lifts small amplitudes");
        assert_ne!(low, high);
        // Solid: always the sand; Note: whatever the note color is.
        let solid = build(Colorize::Solid);
        assert_eq!(
            solid.cell(input(0.7, 0.3, 0.3, 0.3, 0.3), None),
            solid.sand()
        );
        let note = build(Colorize::Note);
        let marker = Some(TermColor::Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(note.cell(input(0.7, 0.3, 0.3, 0.3, 0.3), marker), marker);
    }

    #[test]
    fn note_colors_crossfade_and_fall_back_to_the_sand() {
        let mut scheme = ColorScheme::builtin_default();
        scheme.sand = rgb("#808080");
        scheme.note_colors[0] = rgb("#000000");
        scheme.note_colors[9] = rgb("#ffffff");
        let palette = Palette::new(&scheme, ColorLevel::TrueColor, 24);
        assert_eq!(palette.note(Some(0), Some(0), 0.5), truecolor("#000000"));
        assert_eq!(palette.note(Some(0), Some(9), 0.0), truecolor("#000000"));
        assert_eq!(palette.note(Some(0), Some(9), 1.0), truecolor("#ffffff"));
        let TermColor::Rgb { r, .. } = palette.note(Some(0), Some(9), 0.5).unwrap() else {
            panic!("blended RGB");
        };
        assert!((90..=110).contains(&r), "{r}");
        // Before any note is heard the plate has the sand color, and a first
        // note fades in from it.
        assert_eq!(palette.note(None, None, 1.0), truecolor("#808080"));
        assert_eq!(palette.note(None, Some(0), 0.0), truecolor("#808080"));
        assert_eq!(palette.note(None, Some(0), 1.0), truecolor("#000000"));
        // Palette colors cannot blend: they switch halfway.
        let ansi = Palette::new(&ColorScheme::builtin_default(), ColorLevel::TrueColor, 24);
        assert_eq!(
            ansi.note(Some(0), Some(2), 0.4),
            Some(TermColor::AnsiValue(1))
        );
        assert_eq!(
            ansi.note(Some(0), Some(2), 0.6),
            Some(TermColor::AnsiValue(3))
        );
    }

    #[test]
    fn interface_colors_are_converted_once_and_default_stays_the_terminals() {
        let scheme = ColorScheme::builtin_default();
        let palette = Palette::new(&scheme, ColorLevel::Ansi256, 24);
        assert_eq!(
            palette.background, None,
            "the background is never painted by default"
        );
        assert_eq!(palette.sand(), None);
        assert_eq!(palette.border, Some(TermColor::AnsiValue(8)));
        assert_eq!(palette.title, Some(TermColor::AnsiValue(15)));
        assert_eq!(palette.hud_error, Some(TermColor::AnsiValue(1)));
        assert_eq!(palette.colorize(), Colorize::Density);
        assert_eq!(palette.level(), ColorLevel::Ansi256);
    }
}
