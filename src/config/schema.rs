//! Typed configuration and validation, independent of runtime backends.

use std::{collections::BTreeMap, fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use super::{ConfigError, color::Color, template};

macro_rules! config_enum {
    ($name:ident, $default:ident $(, $variant:ident)* $(,)?) => {
        #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
        #[serde(rename_all = "lowercase")]
        pub enum $name {
            #[default]
            $default,
            $($variant,)*
        }
    };
}

config_enum!(InputMethod, Auto, Pulse, Pipewire, Alsa, Fifo, Stdin, Demo);
config_enum!(ChannelMix, Average, Left, Right);
config_enum!(SampleFormat, S16le, S32le, F32le);
config_enum!(Shape, Square, Circle);
config_enum!(Edge, Free, Fixed);
config_enum!(Symmetry, Anti, Sym, Mixed);
config_enum!(MappingMode, Note, Frequency, Spectrum);
config_enum!(Glyphs, Auto, Braille, Quadrant, Half, Ascii);
config_enum!(Fit, Square, Stretch);
config_enum!(Alignment, Center, Top, Bottom, Left, Right);
config_enum!(BorderStyle, None, Rounded, Square, Double, Thick, Circle);
config_enum!(TitleAlign, Center, Left, Right);
config_enum!(HudPosition, Bottom, Top);
config_enum!(
    Colorize, Density, Solid, Radial, Vertical, Horizontal, Amplitude, Note
);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    #[default]
    Auto,
    Truecolor,
    #[serde(rename = "256")]
    #[value(name = "256")]
    Ansi256,
    #[serde(rename = "16")]
    #[value(name = "16")]
    Ansi16,
    None,
}

/// The largest reachable mode order, or a limit computed from the plate size.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum MaxOrder {
    #[default]
    Auto,
    Fixed(u8),
}

/// An explicit particle count, or a count computed from density and plate size.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ParticleCount {
    #[default]
    Auto,
    Fixed(usize),
}

macro_rules! auto_integer {
    ($name:ident, $integer:ty, $expected:literal, $valid:expr, $deserialize_valid:expr) => {
        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    Self::Auto => formatter.write_str("auto"),
                    Self::Fixed(value) => value.fmt(formatter),
                }
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                if value == "auto" {
                    return Ok(Self::Auto);
                }
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err($expected.to_owned());
                }
                let number = value
                    .parse::<$integer>()
                    .map_err(|_| $expected.to_owned())?;
                if !($valid)(number) {
                    return Err($expected.to_owned());
                }
                Ok(Self::Fixed(number))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match self {
                    Self::Auto => serializer.serialize_str("auto"),
                    Self::Fixed(value) => value.serialize(serializer),
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct AutoOrInteger;

                impl de::Visitor<'_> for AutoOrInteger {
                    type Value = $name;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        formatter.write_str($expected)
                    }

                    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                        if value == "auto" {
                            Ok($name::Auto)
                        } else {
                            Err(E::custom($expected))
                        }
                    }

                    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                        let number =
                            <$integer>::try_from(value).map_err(|_| E::custom($expected))?;
                        if !($deserialize_valid)(number) {
                            return Err(E::custom($expected));
                        }
                        Ok($name::Fixed(number))
                    }

                    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                        self.visit_u64(u64::try_from(value).map_err(|_| E::custom($expected))?)
                    }
                }

                deserializer.deserialize_any(AutoOrInteger)
            }
        }
    };
}

auto_integer!(
    MaxOrder,
    u8,
    "expected auto or an integer in 4..16",
    |value| (4..=16).contains(&value),
    |value| (4..=16).contains(&value)
);
auto_integer!(
    ParticleCount,
    usize,
    "expected auto or a positive integer",
    |value| value > 0,
    |_| true
);

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub input: InputConfig,
    pub analysis: AnalysisConfig,
    pub plate: PlateConfig,
    pub mapping: MappingConfig,
    pub particles: ParticlesConfig,
    pub render: RenderConfig,
    pub layout: LayoutConfig,
    pub border: BorderConfig,
    pub hud: HudConfig,
    pub colors: ColorsConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub framerate: u32,
    pub autosens: bool,
    pub sensitivity: f32,
    pub sleep_timer: u32,
    pub live_reload: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            framerate: 60,
            autosens: true,
            sensitivity: 1.0,
            sleep_timer: 0,
            live_reload: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    pub method: InputMethod,
    pub source: String,
    pub channel_mix: ChannelMix,
    pub sample_rate: u32,
    pub sample_format: SampleFormat,
    pub channels: u16,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            method: InputMethod::Auto,
            source: "auto".to_owned(),
            channel_mix: ChannelMix::Average,
            sample_rate: 48_000,
            sample_format: SampleFormat::S16le,
            channels: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalysisConfig {
    pub fft_size: usize,
    pub low_cutoff: f32,
    pub high_cutoff: f32,
    pub noise_gate: f32,
    pub attack_ms: u32,
    pub release_ms: u32,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            fft_size: 4096,
            low_cutoff: 40.0,
            high_cutoff: 8000.0,
            noise_gate: 0.02,
            attack_ms: 20,
            release_ms: 250,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlateConfig {
    pub shape: Shape,
    pub edge: Edge,
    pub symmetry: Symmetry,
    pub max_order: MaxOrder,
    pub rotate_speed: f32,
}

impl Default for PlateConfig {
    fn default() -> Self {
        Self {
            shape: Shape::Square,
            edge: Edge::Free,
            symmetry: Symmetry::Anti,
            max_order: MaxOrder::Auto,
            rotate_speed: 0.05,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MappingConfig {
    pub mode: MappingMode,
    pub octave_complexity: bool,
    pub base_octave: i8,
    pub min_confidence: f32,
    pub hold_ms: u32,
    pub transition_ms: u32,
    pub peaks: usize,
    pub notes: BTreeMap<String, [u8; 2]>,
}

impl Default for MappingConfig {
    fn default() -> Self {
        Self {
            mode: MappingMode::Note,
            octave_complexity: true,
            base_octave: 2,
            min_confidence: 0.25,
            hold_ms: 120,
            transition_ms: 350,
            peaks: 4,
            notes: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParticlesConfig {
    pub count: ParticleCount,
    pub density: f32,
    pub step: f32,
    pub jitter: f32,
    pub settle: f32,
    pub inertia: f32,
    pub kick: f32,
}

impl Default for ParticlesConfig {
    fn default() -> Self {
        Self {
            count: ParticleCount::Auto,
            density: 0.45,
            step: 1.2,
            jitter: 1.0,
            settle: 0.4,
            inertia: 0.0,
            kick: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderConfig {
    pub glyphs: Glyphs,
    pub ascii_ramp: String,
    pub dot_threshold: f32,
    pub trail: f32,
    pub color_mode: ColorMode,
    pub gradient_steps: usize,
    pub synchronized_output: bool,
    pub cell_aspect: f32,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            glyphs: Glyphs::Auto,
            ascii_ramp: " .:-=+*#%@".to_owned(),
            dot_threshold: 1.0,
            trail: 0.0,
            color_mode: ColorMode::Auto,
            gradient_steps: 24,
            synchronized_output: true,
            cell_aspect: 0.5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutConfig {
    pub fit: Fit,
    pub padding: [u16; 2],
    pub align: Alignment,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            fit: Fit::Square,
            padding: [1, 2],
            align: Alignment::Center,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BorderConfig {
    pub style: BorderStyle,
    pub title: String,
    pub title_align: TitleAlign,
}

impl Default for BorderConfig {
    fn default() -> Self {
        Self {
            style: BorderStyle::None,
            title: String::new(),
            title_align: TitleAlign::Center,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HudConfig {
    pub enabled: bool,
    pub position: HudPosition,
    pub format: String,
}

impl Default for HudConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            position: HudPosition::Bottom,
            format: "{note}{octave}  {mode}  {fps} fps".to_owned(),
        }
    }
}

/// Theme selection and explicit overrides; absent overrides preserve the theme.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorsConfig {
    pub theme: String,
    pub pywal_path: Option<PathBuf>,
    pub background: Option<Color>,
    pub sand: Option<Color>,
    pub colorize: Option<Colorize>,
    pub gradient: Option<Vec<Color>>,
    pub note_colors: Option<[Color; 12]>,
    pub border: Option<Color>,
    pub title: Option<Color>,
    pub hud_text: Option<Color>,
    pub hud_accent: Option<Color>,
    pub hud_dim: Option<Color>,
    pub hud_error: Option<Color>,
}

impl Default for ColorsConfig {
    fn default() -> Self {
        Self {
            theme: "default".to_owned(),
            pywal_path: None,
            background: None,
            sand: None,
            colorize: None,
            gradient: None,
            note_colors: None,
            border: None,
            title: None,
            hud_text: None,
            hud_accent: None,
            hud_dim: None,
            hud_error: None,
        }
    }
}

impl Config {
    /// Rejects invalid values before they can reach an audio or rendering loop.
    pub fn validate(&self) -> Result<(), ConfigError> {
        require(
            self.general.framerate > 0,
            "general.framerate",
            "must be a positive integer",
        )?;
        nonnegative(self.general.sensitivity, "general.sensitivity")?;
        require(
            !self.input.source.trim().is_empty(),
            "input.source",
            "must not be empty; use auto for the default monitor",
        )?;
        require(
            self.input.sample_rate > 0,
            "input.sample_rate",
            "must be a positive integer in Hz",
        )?;
        require(
            self.input.channels > 0,
            "input.channels",
            "must be a positive integer",
        )?;
        require(
            self.analysis.fft_size >= 2 && self.analysis.fft_size.is_power_of_two(),
            "analysis.fft_size",
            "must be a power of two of at least 2",
        )?;
        nonnegative(self.analysis.low_cutoff, "analysis.low_cutoff")?;
        positive(self.analysis.high_cutoff, "analysis.high_cutoff")?;
        require(
            self.analysis.high_cutoff > self.analysis.low_cutoff,
            "analysis.high_cutoff",
            "must be greater than analysis.low_cutoff",
        )?;
        nonnegative(self.analysis.noise_gate, "analysis.noise_gate")?;
        if let MaxOrder::Fixed(order) = self.plate.max_order {
            require(
                (4..=16).contains(&order),
                "plate.max_order",
                "expected auto or an integer in 4..16",
            )?;
        }
        require(
            self.plate.rotate_speed.is_finite(),
            "plate.rotate_speed",
            "must be a finite number in radians per second",
        )?;
        unit_interval(self.mapping.min_confidence, "mapping.min_confidence")?;
        require(
            (1..=8).contains(&self.mapping.peaks),
            "mapping.peaks",
            "must be an integer in 1..8",
        )?;
        self.validate_note_pins()?;
        if let ParticleCount::Fixed(count) = self.particles.count {
            require(
                count > 0,
                "particles.count",
                "expected auto or a positive integer",
            )?;
        }
        positive(self.particles.density, "particles.density")?;
        for (value, key) in [
            (self.particles.step, "particles.step"),
            (self.particles.jitter, "particles.jitter"),
            (self.particles.settle, "particles.settle"),
            (self.particles.kick, "particles.kick"),
        ] {
            nonnegative(value, key)?;
        }
        unit_interval(self.particles.inertia, "particles.inertia")?;
        require(
            !self.render.ascii_ramp.is_empty()
                && self
                    .render
                    .ascii_ramp
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() || byte == b' '),
            "render.ascii_ramp",
            "must contain at least one printable ASCII character (spaces are allowed)",
        )?;
        positive(self.render.dot_threshold, "render.dot_threshold")?;
        unit_interval(self.render.trail, "render.trail")?;
        require(
            self.render.gradient_steps > 0,
            "render.gradient_steps",
            "must be a positive integer",
        )?;
        positive(self.render.cell_aspect, "render.cell_aspect")?;
        require(
            self.border.style != BorderStyle::Circle || self.plate.shape == Shape::Circle,
            "border.style",
            "circle requires plate.shape = circle",
        )?;
        for (template_text, key) in [
            (&self.hud.format, "hud.format"),
            (&self.border.title, "border.title"),
        ] {
            template::parse(template_text).map_err(|error| ConfigError::Validation {
                key: key.to_owned(),
                message: error.to_string(),
            })?;
        }
        require(
            !self.colors.theme.trim().is_empty(),
            "colors.theme",
            "must be a theme name, theme file, or pywal",
        )?;
        if let Some(path) = &self.colors.pywal_path {
            require(
                !path.as_os_str().is_empty(),
                "colors.pywal_path",
                "must be a nonempty path to colors.json",
            )?;
        }
        if let Some(gradient) = &self.colors.gradient {
            require(
                !gradient.is_empty(),
                "colors.gradient",
                "must contain at least one color",
            )?;
        }
        Ok(())
    }

    fn validate_note_pins(&self) -> Result<(), ConfigError> {
        const NOTES: [&str; 12] = [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ];
        for (note, &[n, second]) in &self.mapping.notes {
            let key = format!("mapping.notes.{note}");
            require(
                NOTES.contains(&note.as_str()),
                &key,
                "expected a pitch class: C, C#, D, D#, E, F, F#, G, G#, A, A#, B",
            )?;
            match self.plate.shape {
                Shape::Square => {
                    require(
                        n <= 16 && second <= 16,
                        &key,
                        "square mode orders n and m must be in 0..16",
                    )?;
                    require(
                        self.plate.edge != Edge::Fixed || (n >= 1 && second >= 1),
                        &key,
                        "fixed square edges require both mode orders to be in 1..16",
                    )?;
                    require(
                        self.plate.symmetry != Symmetry::Anti || n != second,
                        &key,
                        "anti symmetry requires different square mode orders (n != m)",
                    )?;
                }
                Shape::Circle => {
                    require(
                        n <= 16 && (1..=8).contains(&second),
                        &key,
                        "circular drum modes require n in 0..16 and k in 1..8",
                    )?;
                }
            }
        }
        Ok(())
    }
}

fn require(valid: bool, key: &str, message: &str) -> Result<(), ConfigError> {
    if valid {
        Ok(())
    } else {
        Err(ConfigError::Validation {
            key: key.to_owned(),
            message: message.to_owned(),
        })
    }
}

fn positive(value: f32, key: &str) -> Result<(), ConfigError> {
    require(
        value.is_finite() && value > 0.0,
        key,
        "must be a finite number greater than zero",
    )
}

fn nonnegative(value: f32, key: &str) -> Result<(), ConfigError> {
    require(
        value.is_finite() && value >= 0.0,
        key,
        "must be a finite number greater than or equal to zero",
    )
}

fn unit_interval(value: f32, key: &str) -> Result<(), ConfigError> {
    require(
        value.is_finite() && (0.0..=1.0).contains(&value),
        key,
        "must be a finite number in 0..1",
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn embedded_defaults_match_and_round_trip() {
        let config: Config = toml::from_str(include_str!("../../assets/config.toml")).unwrap();
        assert_eq!(config, Config::default());
        config.validate().unwrap();
        let serialized = toml::to_string_pretty(&config).unwrap();
        assert_eq!(toml::from_str::<Config>(&serialized).unwrap(), config);
        assert_eq!(toml::from_str::<Config>("").unwrap(), config);
    }

    #[test]
    fn partial_sections_preserve_unspecified_defaults() {
        let config: Config =
            toml::from_str("[general]\nframerate = 120\n[colors]\nsand = '#bada55'\n").unwrap();
        let mut expected = Config::default();
        expected.general.framerate = 120;
        expected.colors.sand = Some(Color::Rgb {
            r: 186,
            g: 218,
            b: 85,
        });
        assert_eq!(config, expected);
    }

    #[test]
    fn integer_or_auto_fields_use_distinct_toml_types_and_clear_errors() {
        let config: Config =
            toml::from_str("[plate]\nmax_order = 16\n[particles]\ncount = 50000").unwrap();
        assert_eq!(config.plate.max_order, MaxOrder::Fixed(16));
        assert_eq!(config.particles.count, ParticleCount::Fixed(50_000));
        assert_eq!(
            "auto".parse::<ParticleCount>().unwrap(),
            ParticleCount::Auto
        );
        assert_eq!(
            "2500".parse::<ParticleCount>().unwrap(),
            ParticleCount::Fixed(2500)
        );
        for invalid in ["0", "-1", "+1", "2.5", "AUTO", ""] {
            assert!(
                invalid
                    .parse::<ParticleCount>()
                    .unwrap_err()
                    .contains("positive integer")
            );
        }
        for invalid in ["0", "3", "17", "'8'", "'bad'", "1.5"] {
            let text = format!("[plate]\nmax_order = {invalid}");
            assert!(
                toml::from_str::<Config>(&text)
                    .unwrap_err()
                    .to_string()
                    .contains("auto or an integer in 4..16")
            );
        }
    }

    #[test]
    fn validation_names_the_invalid_key() {
        for (text, key) in [
            ("[general]\nframerate = 0", "general.framerate"),
            ("[general]\nsensitivity = nan", "general.sensitivity"),
            ("[input]\nchannels = 0", "input.channels"),
            ("[input]\nsample_rate = 0", "input.sample_rate"),
            ("[analysis]\nfft_size = 4095", "analysis.fft_size"),
            ("[analysis]\nlow_cutoff = 8000", "analysis.high_cutoff"),
            ("[mapping]\nmin_confidence = 1.1", "mapping.min_confidence"),
            ("[mapping]\npeaks = 9", "mapping.peaks"),
            ("[particles]\ndensity = inf", "particles.density"),
            ("[particles]\ncount = 0", "particles.count"),
            ("[particles]\ninertia = -0.1", "particles.inertia"),
            ("[render]\ntrail = 1.1", "render.trail"),
            ("[render]\ncell_aspect = 0", "render.cell_aspect"),
            ("[render]\nascii_ramp = ''", "render.ascii_ramp"),
            ("[render]\ngradient_steps = 0", "render.gradient_steps"),
            ("[border]\nstyle = 'circle'", "border.style"),
            ("[border]\ntitle = '{pitch}'", "border.title"),
            ("[hud]\nformat = '{note'", "hud.format"),
            ("[hud]\nformat = 'a}'", "hud.format"),
            ("[colors]\ngradient = []", "colors.gradient"),
        ] {
            let config: Config = toml::from_str(text).unwrap();
            assert!(
                matches!(config.validate(), Err(ConfigError::Validation { key: found, .. }) if found == key),
                "{text}"
            );
        }
    }

    #[test]
    fn raw_pcm_accepts_sample_rates_below_the_configured_upper_cutoff() {
        let mut config = Config::default();
        config.input.method = InputMethod::Stdin;
        for sample_rate in [8_000, 44_100, 48_000] {
            config.input.sample_rate = sample_rate;
            config.validate().unwrap();
        }
        config.general.sensitivity = 0.0;
        config.validate().unwrap();
    }

    #[test]
    fn note_pins_follow_plate_boundaries_and_symmetry() {
        let mut config = Config::default();
        config.mapping.notes.insert("C".to_owned(), [0, 3]);
        config.validate().unwrap();
        config.plate.edge = Edge::Fixed;
        assert!(config.validate().is_err());
        config.mapping.notes.insert("C".to_owned(), [3, 3]);
        assert!(config.validate().is_err());
        config.plate.symmetry = Symmetry::Sym;
        config.validate().unwrap();
        config.plate.shape = Shape::Circle;
        config.mapping.notes.insert("C".to_owned(), [0, 8]);
        config.validate().unwrap();
        config.mapping.notes.insert("C".to_owned(), [0, 9]);
        assert!(config.validate().is_err());
        config.mapping.notes.clear();
        config.mapping.notes.insert("H".to_owned(), [0, 1]);
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("mapping.notes.H")
        );
    }

    #[test]
    fn colors_override_round_trip_without_materializing_a_theme() {
        let config: Config = toml::from_str("[colors]\nbackground = 'default'\ncolorize = 'note'\ngradient = ['8', '#ffffff']\nnote_colors = ['0','1','2','3','4','5','6','7','8','9','10','11']\npywal_path = '/tmp/colors.json'").unwrap();
        config.validate().unwrap();
        assert_eq!(config.colors.background, Some(Color::Default));
        assert_eq!(config.colors.sand, None);
        assert_eq!(
            toml::from_str::<Config>(&toml::to_string(&config).unwrap()).unwrap(),
            config
        );
        assert!(toml::from_str::<Config>("[colors]\nnote_colors = ['0']").is_err());
    }
}
