//! The status line, short-lived notices, and the help overlay.

use std::fmt::{self, Write as _};

use crossterm::style::Color;

use crate::{
    chladni::{Mode, mapping::NOTE_NAMES},
    config::{
        schema::Shape,
        template::{Field, Segment},
    },
};

use super::{
    border::{self, BorderChars},
    colorize::Palette,
    frame::{Cell, Frame},
};

/// What the status line can say.
#[derive(Debug, Clone, Copy)]
pub struct StatusValues<'a> {
    /// Pitch class (0 = C) and octave of the last pitch heard.
    pub note: Option<(u8, i8)>,
    pub hz: Option<f32>,
    pub mode: Option<Mode>,
    pub shape: Shape,
    pub theme: &'a str,
    pub fps: f32,
    pub particles: usize,
    pub source: &'a str,
    /// The energy meter, already drawn (see [`EnergyMeter::write`]).
    pub energy: &'a str,
}

/// Which color a stretch of text gets: fixed words or looked-up values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emphasis {
    Plain,
    Value,
    /// Secondary status such as "no audio".
    Dim,
}

/// The colors a line is drawn in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineColors {
    pub plain: Option<Color>,
    pub value: Option<Color>,
    pub dim: Option<Color>,
}

/// A line of text in two colors. It reuses its storage, so building one every
/// frame allocates only while it grows.
#[derive(Debug, Default, Clone)]
pub struct TextLine {
    text: String,
    spans: Vec<(usize, usize, Emphasis)>,
}

impl TextLine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.spans.clear();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn push(&mut self, text: &str, emphasis: Emphasis) {
        let start = self.text.len();
        self.text.push_str(text);
        self.span(start, emphasis);
    }

    fn push_fmt(&mut self, emphasis: Emphasis, arguments: fmt::Arguments<'_>) {
        let start = self.text.len();
        // Writing to a String cannot fail.
        let _ = self.text.write_fmt(arguments);
        self.span(start, emphasis);
    }

    fn span(&mut self, start: usize, emphasis: Emphasis) {
        let end = self.text.len();
        if end > start {
            self.spans.push((start, end, emphasis));
        }
    }
}

/// The name of pitch class `class` (0 = C), such as `A#`.
pub fn note_name(class: u8) -> &'static str {
    NOTE_NAMES.get(usize::from(class)).copied().unwrap_or("?")
}

/// Expand a parsed template into `line`. A pitch that has not been heard shows
/// as `-`.
pub fn render_template(line: &mut TextLine, segments: &[Segment], values: &StatusValues<'_>) {
    line.clear();
    for segment in segments {
        match segment {
            Segment::Literal(text) => line.push(text, Emphasis::Plain),
            Segment::Field(field) => {
                let value = Emphasis::Value;
                match field {
                    Field::Note => match values.note {
                        Some((class, _)) => line.push(note_name(class), value),
                        None => line.push("-", value),
                    },
                    Field::Octave => {
                        if let Some((_, octave)) = values.note {
                            line.push_fmt(value, format_args!("{octave}"));
                        }
                    }
                    Field::Hz => match values.hz {
                        Some(hz) => line.push_fmt(value, format_args!("{hz:.0}")),
                        None => line.push("-", value),
                    },
                    Field::Mode => match values.mode {
                        Some(mode) => line.push_fmt(value, format_args!("{mode}")),
                        None => line.push("-", value),
                    },
                    Field::Shape => line.push(
                        match values.shape {
                            Shape::Square => "square",
                            Shape::Circle => "circle",
                        },
                        value,
                    ),
                    Field::Theme => line.push(values.theme, value),
                    Field::Fps => line.push_fmt(value, format_args!("{:.0}", values.fps)),
                    Field::Particles => line.push_fmt(value, format_args!("{}", values.particles)),
                    Field::Source => line.push(values.source, value),
                    Field::Energy => line.push(values.energy, value),
                }
            }
        }
    }
}

/// Write `line` into row `y` from column `x`, at most `width` cells wide,
/// keeping whatever background the cells have. Returns the cells used.
pub fn draw_line(
    frame: &mut Frame,
    x: u16,
    y: u16,
    width: u16,
    line: &TextLine,
    colors: LineColors,
) -> u16 {
    let mut column = 0;
    for &(start, end, emphasis) in &line.spans {
        let color = match emphasis {
            Emphasis::Plain => colors.plain,
            Emphasis::Value => colors.value,
            Emphasis::Dim => colors.dim,
        };
        for character in line.text[start..end].chars() {
            if column >= width {
                return column;
            }
            if let Some(cell) = frame.cell_mut(x + column, y) {
                *cell = Cell {
                    character,
                    fg: color,
                    bg: cell.bg,
                };
            }
            column += 1;
        }
    }
    column
}

/// A short history of the energy as a row of `▁▂▃▄▅▆▇█`, newest on the right.
#[derive(Debug, Clone)]
pub struct EnergyMeter {
    levels: [f32; Self::WIDTH],
    peak: f32,
    elapsed: f32,
}

impl Default for EnergyMeter {
    fn default() -> Self {
        Self {
            levels: [0.0; Self::WIDTH],
            peak: 0.0,
            elapsed: 0.0,
        }
    }
}

impl EnergyMeter {
    pub const WIDTH: usize = 8;
    /// Seconds per column.
    const INTERVAL: f32 = 0.1;
    /// The energy that fills a column.
    const FULL_SCALE: f32 = 1.5;
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    /// What stands in for the blocks where there is no Unicode font.
    const ASCII: [char; 8] = ['.', ':', '-', '=', '+', '*', '#', '@'];

    /// Account for `dt` seconds at `energy`; every column shows the peak of
    /// its interval, so a short kick is not averaged away.
    pub fn update(&mut self, energy: f32, dt: f32) {
        if !energy.is_finite() || !dt.is_finite() || dt < 0.0 {
            return;
        }
        self.peak = self.peak.max(energy);
        self.elapsed += dt;
        while self.elapsed >= Self::INTERVAL {
            self.elapsed -= Self::INTERVAL;
            self.levels.rotate_left(1);
            self.levels[Self::WIDTH - 1] = self.peak;
            self.peak = 0.0;
        }
    }

    /// Whether the meter shows nothing and has nothing waiting to show.
    pub fn is_flat(&self) -> bool {
        self.peak == 0.0 && self.levels.iter().all(|&level| level == 0.0)
    }

    pub fn write(&self, unicode: bool, out: &mut String) {
        let glyphs = if unicode { &Self::BLOCKS } else { &Self::ASCII };
        for &level in &self.levels {
            let index = (level / Self::FULL_SCALE * 7.0).round().clamp(0.0, 7.0) as usize;
            out.push(glyphs[index]);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Error,
}

/// How long a notice stays, in seconds.
pub const NOTICE_SECONDS: f32 = 4.0;

/// A message shown on the status row for a few seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
    remaining: f32,
}

impl Notice {
    pub fn new(kind: NoticeKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            remaining: NOTICE_SECONDS,
        }
    }

    /// Advance by `dt`; false once the notice has expired.
    pub fn tick(&mut self, dt: f32) -> bool {
        if dt.is_finite() && dt > 0.0 {
            self.remaining -= dt;
        }
        self.remaining > 0.0
    }
}

/// The keys of the interface, as shown by `?`.
pub fn help_lines(unicode: bool) -> [(&'static str, &'static str); 14] {
    let (up_down, left_right) = if unicode {
        ("↑ ↓", "← →")
    } else {
        ("Up Down", "Left Right")
    };
    [
        ("q Esc Ctrl-C", "quit"),
        ("Space", "freeze the sand"),
        ("s", "scatter the sand"),
        (up_down, "sensitivity up / down"),
        (left_right, "simpler / more intricate figures"),
        ("t T", "next / previous theme"),
        ("p", "square / circle plate"),
        ("m", "cycle the mapping mode"),
        ("g", "cycle the glyph set"),
        ("i", "show / hide the HUD"),
        ("r", "reload the configuration"),
        ("c", "reload the colors only"),
        ("?", "this help"),
        ("any key", "closes it"),
    ]
}

/// The size in cells of the help box.
pub fn help_size(unicode: bool) -> (u16, u16) {
    let lines = help_lines(unicode);
    let keys = lines
        .iter()
        .map(|(keys, _)| keys.chars().count())
        .max()
        .unwrap_or(0);
    let words = lines
        .iter()
        .map(|(_, text)| text.chars().count())
        .max()
        .unwrap_or(0);
    // Border, one cell of padding, keys, a gap, words; title, gap and lines.
    ((keys + words + 7) as u16, lines.len() as u16 + 4)
}

/// Draw the help box in the middle of the frame. Returns false, drawing
/// nothing, if the frame is too small for it.
pub fn draw_help(frame: &mut Frame, palette: &Palette, chars: BorderChars, unicode: bool) -> bool {
    let (width, height) = help_size(unicode);
    if frame.width() < width || frame.height() < height {
        return false;
    }
    let (x, y) = ((frame.width() - width) / 2, (frame.height() - height) / 2);
    // Clear the box so that the sand does not show through.
    for row in y..y + height {
        for column in x..x + width {
            if let Some(cell) = frame.cell_mut(column, row) {
                *cell = Cell {
                    character: ' ',
                    fg: None,
                    bg: cell.bg,
                };
            }
        }
    }
    let rect = border::Rect {
        x,
        y,
        width,
        height,
    };
    border::draw_box(frame, rect, chars, palette.border);
    let inner = width - 2;
    border::draw_aligned(
        frame,
        x + 1,
        y + 1,
        inner,
        "grava keys",
        crate::config::schema::TitleAlign::Center,
        palette.title,
    );
    let keys_width = help_lines(unicode)
        .iter()
        .map(|(keys, _)| keys.chars().count())
        .max()
        .unwrap_or(0) as u16;
    for (index, (keys, words)) in help_lines(unicode).iter().enumerate() {
        let row = y + 3 + index as u16;
        let mut line = TextLine::new();
        line.push(keys, Emphasis::Value);
        let colors = LineColors {
            plain: palette.hud_text,
            value: palette.hud_accent,
            dim: palette.hud_dim,
        };
        draw_line(frame, x + 2, row, keys_width, &line, colors);
        let mut line = TextLine::new();
        let closing = index + 1 == help_lines(unicode).len();
        line.push(
            words,
            if closing {
                Emphasis::Dim
            } else {
                Emphasis::Plain
            },
        );
        draw_line(
            frame,
            x + 2 + keys_width + 2,
            row,
            inner.saturating_sub(keys_width + 3),
            &line,
            colors,
        );
    }
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{
        chladni::{CircleMode, SquareMode},
        config::{
            schema::{BorderStyle, Edge, Symmetry},
            template,
            theme::ColorScheme,
        },
        render::color::ColorLevel,
    };

    fn values<'a>() -> StatusValues<'a> {
        StatusValues {
            note: Some((10, 4)),
            hz: Some(466.16),
            mode: Some(Mode::Square(
                SquareMode::new(2, 5, Edge::Free, Symmetry::Anti).unwrap(),
            )),
            shape: Shape::Square,
            theme: "nord",
            fps: 59.6,
            particles: 12_345,
            source: "demo",
            energy: "▁▃▅█",
        }
    }

    fn expand(format: &str, values: &StatusValues<'_>) -> String {
        let mut line = TextLine::new();
        render_template(&mut line, &template::parse(format).unwrap(), values);
        line.text().to_owned()
    }

    #[test]
    fn every_placeholder_shows_its_value() {
        let values = values();
        assert_eq!(
            expand("{note}{octave}  {mode}  {fps} fps", &values),
            "A#4  (2,5)  60 fps"
        );
        assert_eq!(
            expand("{hz} Hz {shape} {theme}", &values),
            "466 Hz square nord"
        );
        assert_eq!(
            expand("{particles} {source} {energy}", &values),
            "12345 demo ▁▃▅█"
        );
        assert_eq!(expand("{{{note}}}", &values), "{A#}");
        let drum = StatusValues {
            mode: Some(Mode::Circle(CircleMode::new(3, 2).unwrap())),
            shape: Shape::Circle,
            ..values
        };
        assert_eq!(expand("{mode} {shape}", &drum), "J(3,2) circle");
    }

    #[test]
    fn an_unheard_pitch_shows_dashes_instead_of_nonsense() {
        let silent = StatusValues {
            note: None,
            hz: None,
            mode: None,
            ..values()
        };
        assert_eq!(expand("{note}{octave} {hz} {mode}", &silent), "- - -");
        for class in 0..12 {
            assert!(!note_name(class).is_empty());
        }
        assert_eq!(note_name(0), "C");
        assert_eq!(note_name(11), "B");
        assert_eq!(note_name(12), "?");
    }

    #[test]
    fn values_and_fixed_text_get_their_own_colors_and_long_lines_are_cut() {
        let mut line = TextLine::new();
        render_template(
            &mut line,
            &template::parse("note {note}!").unwrap(),
            &values(),
        );
        let mut frame = Frame::new(12, 2);
        let colors = LineColors {
            plain: Some(Color::Red),
            value: Some(Color::Blue),
            dim: Some(Color::Green),
        };
        let used = draw_line(&mut frame, 1, 1, 12, &line, colors);
        assert_eq!(used, 8);
        assert_eq!(frame.plain_text().lines().nth(1).unwrap(), " note A#!   ");
        let fg = |x| frame.cell(x, 1).unwrap().fg;
        assert_eq!(
            (fg(1), fg(5), fg(6), fg(7), fg(8)),
            (
                Some(Color::Red),
                Some(Color::Red),
                Some(Color::Blue),
                Some(Color::Blue),
                Some(Color::Red)
            )
        );
        // Cut to the width given, and clipped by the frame without panicking.
        let mut frame = Frame::new(12, 2);
        let none = LineColors::default();
        assert_eq!(draw_line(&mut frame, 0, 0, 4, &line, none), 4);
        assert_eq!(frame.plain_text().lines().next().unwrap(), "note        ");
        assert_eq!(draw_line(&mut Frame::new(3, 1), 0, 0, 50, &line, none), 8);
        draw_line(&mut Frame::new(3, 1), 5, 5, 5, &line, none);
        // The background of the cells underneath is kept.
        let mut frame = Frame::new(4, 1);
        frame.cell_mut(0, 0).unwrap().bg = Some(Color::Green);
        draw_line(&mut frame, 0, 0, 4, &line, none);
        assert_eq!(frame.cell(0, 0).unwrap().bg, Some(Color::Green));
        // Dim text has its own color.
        let mut dim = TextLine::new();
        dim.push("no audio", Emphasis::Dim);
        let mut frame = Frame::new(8, 1);
        draw_line(&mut frame, 0, 0, 8, &dim, colors);
        assert_eq!(frame.cell(0, 0).unwrap().fg, Some(Color::Green));
    }

    #[test]
    fn the_energy_meter_is_flat_only_when_it_shows_and_holds_nothing() {
        let mut meter = EnergyMeter::default();
        assert!(meter.is_flat());
        meter.update(0.0, 1.0);
        assert!(meter.is_flat());
        // A peak waiting for its column counts, and so does a shown level.
        meter.update(0.5, 0.01);
        assert!(!meter.is_flat());
        meter.update(0.0, 0.2);
        assert!(!meter.is_flat());
        // The level scrolls out of the eight columns in 0.8 s.
        for _ in 0..EnergyMeter::WIDTH + 1 {
            meter.update(0.0, EnergyMeter::INTERVAL);
        }
        assert!(meter.is_flat());
    }

    #[test]
    fn the_energy_meter_is_a_peak_history_of_blocks() {
        let mut meter = EnergyMeter::default();
        let mut text = String::new();
        meter.write(true, &mut text);
        assert_eq!(text, "▁▁▁▁▁▁▁▁");
        // 1.5 s of loud sound after a quiet start: the right columns fill.
        for _ in 0..30 {
            meter.update(0.0, 1.0 / 60.0);
        }
        for _ in 0..30 {
            meter.update(1.5, 1.0 / 60.0);
        }
        text.clear();
        meter.write(true, &mut text);
        assert_eq!(text.chars().count(), EnergyMeter::WIDTH);
        assert!(text.ends_with("████"), "{text}");
        assert!(text.starts_with('▁'));
        // A single frame of energy inside a column is not averaged away.
        let mut meter = EnergyMeter::default();
        meter.update(1.5, 1.0 / 60.0);
        for _ in 0..5 {
            meter.update(0.0, 1.0 / 60.0);
        }
        text.clear();
        meter.write(true, &mut text);
        assert!(text.ends_with('█'), "{text}");
        // Levels map onto all eight blocks, and ASCII has its own.
        let mut ramp = EnergyMeter::default();
        for step in 0..8 {
            ramp.update(step as f32 * 1.5 / 7.0, EnergyMeter::INTERVAL);
        }
        text.clear();
        ramp.write(true, &mut text);
        assert_eq!(text, "▁▂▃▄▅▆▇█");
        text.clear();
        ramp.write(false, &mut text);
        assert_eq!(text, ".:-=+*#@");
        // Garbage is ignored.
        ramp.update(f32::NAN, 0.1);
        ramp.update(1.0, f32::INFINITY);
        ramp.update(1.0, -1.0);
    }

    #[test]
    fn notices_expire_after_a_few_seconds() {
        let mut notice = Notice::new(NoticeKind::Error, "broken config");
        assert!(notice.tick(1.0 / 60.0));
        for _ in 0..200 {
            assert!(notice.tick(1.0 / 60.0));
        }
        assert!(!notice.tick(1.0), "gone after about {NOTICE_SECONDS} s");
        let mut stuck = Notice::new(NoticeKind::Info, "x");
        for dt in [f32::NAN, -5.0, 0.0] {
            assert!(stuck.tick(dt));
        }
    }

    #[test]
    fn the_help_box_lists_every_key_centered_and_clears_what_is_under_it() {
        let palette = Palette::new(&ColorScheme::builtin_default(), ColorLevel::TrueColor, 24);
        let chars = border::box_chars(BorderStyle::Rounded, true).unwrap();
        let (width, height) = help_size(true);
        let mut frame = Frame::new(width + 10, height + 4);
        for cell in frame.cells_mut() {
            cell.character = '#';
        }
        assert!(draw_help(&mut frame, &palette, chars, true));
        let text = frame.plain_text();
        for needle in [
            "grava keys",
            "q Esc Ctrl-C",
            "quit",
            "Space",
            "freeze",
            "scatter",
            "↑ ↓",
            "sensitivity",
            "← →",
            "intricate",
            "t T",
            "theme",
            "plate",
            "mapping",
            "glyph",
            "HUD",
            "reload the configuration",
            "colors only",
            "this help",
        ] {
            assert!(text.contains(needle), "{needle:?} missing:\n{text}");
        }
        // Centered, rounded, and nothing of the sand shows inside the box.
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows[2].chars().nth(5), Some('╭'));
        assert!(
            rows[3..rows.len() - 2].iter().all(|row| row
                .chars()
                .skip(6)
                .take(width as usize - 2)
                .all(|c| c != '#')),
            "{text}"
        );
        assert!(rows[0].chars().all(|c| c == '#'));
        // ASCII terminals get ASCII keys and a plain box.
        let ascii = help_lines(false);
        assert!(ascii.iter().any(|(keys, _)| *keys == "Up Down"));
        assert!(
            ascii
                .iter()
                .all(|(keys, words)| keys.is_ascii() && words.is_ascii())
        );
        let mut frame = Frame::new(60, 30);
        let plain = border::box_chars(BorderStyle::Rounded, false).unwrap();
        assert!(draw_help(&mut frame, &palette, plain, false));
        assert!(frame.plain_text().is_ascii());
        // Too small a terminal draws nothing and says so.
        let mut small = Frame::new(width - 1, height + 5);
        assert!(!draw_help(&mut small, &palette, chars, true));
        assert_eq!(small, Frame::new(width - 1, height + 5));
        assert!(!draw_help(
            &mut Frame::new(width + 5, height - 1),
            &palette,
            chars,
            true
        ));
    }
}
