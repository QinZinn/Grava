#![forbid(unsafe_code)]

//! Command-line options and configuration overrides.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::OnceLock;

use clap::{ArgGroup, Parser, ValueEnum};

use crate::config::schema::{ColorMode, Config, Glyphs, InputMethod, ParticleCount, Shape};

/// Command-line interface, including options reserved for later milestones.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "grava",
    about = "A terminal audio visualizer inspired by sand on a vibrating plate",
    long_about = "grava listens to what the computer plays and draws it as sand on a vibrating plate or drum head. Grains are thrown off the parts of the surface that move and settle on the nodal lines where it stays still, so every note makes its own Chladni figure, and the sand flows into a new one when the note changes. It records the monitor of the default output through PulseAudio or PipeWire and needs no configuration.",
    after_help = "Plain `grava` records the monitor of the default output (see --list-sources). Use --input demo, stdin or fifo for built-in or piped audio; add --headless --frames 300 for a deterministic snapshot of those.\n\nKeys: q/Esc quit, space freeze, s scatter, Up/Down sensitivity, Left/Right simpler/more intricate figures, t/T next/previous theme, c reload colors, r reload everything, p square/circle plate, m mapping, g glyphs, i status line, ? help.\nSignals: SIGUSR1 reloads the configuration, SIGUSR2 only the colors.\nWhen the plate is at rest in silence, grava sleeps between 50 ms ticks and uses almost no CPU; general.sleep_timer blanks it after that many seconds of silence.\nShell completions: --completions bash|zsh|fish. Manual page: --man.",
    version = version_text(),
    group(
        ArgGroup::new("action")
            .multiple(false)
            .args([
                "list_sources",
                "list_themes",
                "print_default_config",
                "check_config",
                "init",
                "completions",
                "man",
            ])
    )
)]
pub struct Cli {
    /// Read configuration from this path instead of the XDG default.
    #[arg(short = 'c', long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Select the audio input method.
    #[arg(short = 'i', long, value_enum)]
    pub input: Option<InputMethod>,

    /// Select an audio source, device, or FIFO path.
    #[arg(short = 's', long, value_name = "SOURCE")]
    pub source: Option<String>,

    /// Select a built-in theme, a theme file, or pywal.
    #[arg(short = 't', long, value_name = "NAME")]
    pub theme: Option<String>,

    /// Select the plate shape.
    #[arg(long, value_enum)]
    pub shape: Option<Shape>,

    /// Select the terminal glyph set.
    #[arg(long, value_enum)]
    pub glyphs: Option<Glyphs>,

    /// Set the particle count or derive it from the plate area.
    #[arg(long, value_name = "N|auto")]
    pub particles: Option<ParticleCount>,

    /// Set the target frame rate in frames per second.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub fps: Option<u32>,

    /// Print audio sources for the selected backend.
    #[arg(long)]
    pub list_sources: bool,

    /// Print available themes.
    #[arg(long)]
    pub list_themes: bool,

    /// Print the embedded, fully commented default configuration.
    #[arg(long)]
    pub print_default_config: bool,

    /// Validate configuration and command-line overrides, then exit.
    #[arg(long)]
    pub check_config: bool,

    /// Write the default configuration and a sample theme.
    #[arg(long)]
    pub init: bool,

    /// Replace existing files when initializing configuration.
    #[arg(long, requires = "init")]
    pub force: bool,

    /// Run without a terminal and print the final frame.
    #[arg(long, requires = "frames", conflicts_with = "action")]
    pub headless: bool,

    /// Render this many frames in headless mode.
    #[arg(
        long,
        value_name = "N",
        requires = "headless",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub frames: Option<u64>,

    /// Set headless terminal dimensions in character cells.
    #[arg(long, value_name = "WxH", requires = "headless")]
    pub size: Option<TerminalSize>,

    /// Seed deterministic demo audio and particle motion.
    #[arg(long, value_name = "N")]
    pub seed: Option<u64>,

    /// Disable ANSI colors.
    #[arg(long)]
    pub no_color: bool,

    /// Print per-stage timing information.
    #[arg(long)]
    pub bench: bool,

    /// Generate shell completions.
    #[arg(long, value_enum, value_name = "SHELL")]
    pub completions: Option<CompletionShell>,

    /// Generate a manual page.
    #[arg(long)]
    pub man: bool,
}

impl Cli {
    /// Apply explicitly supplied options after loading the configuration file.
    pub fn apply_overrides(&self, config: &mut Config) {
        if let Some(method) = self.input {
            config.input.method = method;
        }
        if let Some(source) = &self.source {
            config.input.source.clone_from(source);
        }
        if let Some(theme) = &self.theme {
            config.colors.theme.clone_from(theme);
        }
        if let Some(shape) = self.shape {
            config.plate.shape = shape;
        }
        if let Some(glyphs) = self.glyphs {
            config.render.glyphs = glyphs;
        }
        if let Some(count) = self.particles {
            config.particles.count = count;
        }
        if let Some(framerate) = self.fps {
            config.general.framerate = framerate;
        }
        if self.no_color {
            config.render.color_mode = ColorMode::None;
        }
    }
}

/// Shells supported by the completion command.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
}

/// Headless output dimensions, measured in terminal character cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    pub width: u16,
    pub height: u16,
}

impl FromStr for TerminalSize {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (width, height) = value
            .split_once('x')
            .ok_or_else(|| "expected WIDTHxHEIGHT, for example 80x40".to_owned())?;
        let parse_dimension = |dimension: &str, name: &str| {
            dimension
                .parse::<u16>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| format!("{name} must be an integer from 1 to {}", u16::MAX))
        };
        Ok(Self {
            width: parse_dimension(width, "width")?,
            height: parse_dimension(height, "height")?,
        })
    }
}

impl fmt::Display for TerminalSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}x{}", self.width, self.height)
    }
}

fn version_text() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();

    VERSION.get_or_init(|| {
        let features: Vec<_> = [
            (cfg!(feature = "pulse"), "pulse"),
            (cfg!(feature = "pipewire"), "pipewire"),
            (cfg!(feature = "alsa"), "alsa"),
        ]
        .into_iter()
        .filter_map(|(enabled, name)| enabled.then_some(name))
        .collect();
        let enabled = if features.is_empty() {
            "none".to_owned()
        } else {
            features.join(", ")
        };
        format!(
            "{}\nenabled audio features: {enabled}\naudio inputs: the listed capture backends, the demo synth, raw PCM on stdin or a fifo",
            env!("CARGO_PKG_VERSION")
        )
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use clap::error::ErrorKind;

    use super::*;

    #[test]
    fn parses_runtime_options() {
        let cli = Cli::try_parse_from([
            "grava",
            "-c",
            "custom.toml",
            "-i",
            "demo",
            "-s",
            "auto",
            "-t",
            "sand",
            "--shape",
            "circle",
            "--glyphs",
            "braille",
            "--particles",
            "12000",
            "--fps",
            "120",
            "--headless",
            "--frames",
            "300",
            "--size",
            "80x40",
            "--seed",
            "42",
            "--no-color",
            "--bench",
        ])
        .unwrap();

        assert_eq!(cli.config, Some(PathBuf::from("custom.toml")));
        assert_eq!(cli.input, Some(InputMethod::Demo));
        assert_eq!(cli.source.as_deref(), Some("auto"));
        assert_eq!(cli.theme.as_deref(), Some("sand"));
        assert_eq!(cli.shape, Some(Shape::Circle));
        assert_eq!(cli.glyphs, Some(Glyphs::Braille));
        assert_eq!(cli.particles, Some(ParticleCount::Fixed(12000)));
        assert_eq!(cli.fps, Some(120));
        assert!(cli.headless);
        assert_eq!(cli.frames, Some(300));
        assert_eq!(
            cli.size,
            Some(TerminalSize {
                width: 80,
                height: 40,
            })
        );
        assert_eq!(cli.seed, Some(42));
        assert!(cli.no_color);
        assert!(cli.bench);
    }

    #[test]
    fn parses_all_input_methods_and_glyph_sets() {
        for input in ["auto", "pulse", "pipewire", "alsa", "fifo", "stdin", "demo"] {
            assert!(Cli::try_parse_from(["grava", "--input", input]).is_ok());
        }
        for glyphs in ["auto", "braille", "quadrant", "half", "ascii"] {
            assert!(Cli::try_parse_from(["grava", "--glyphs", glyphs]).is_ok());
        }
    }

    #[test]
    fn parses_utility_commands() {
        assert!(
            Cli::try_parse_from(["grava", "--list-sources"])
                .unwrap()
                .list_sources
        );
        assert!(
            Cli::try_parse_from(["grava", "--list-themes"])
                .unwrap()
                .list_themes
        );
        assert!(
            Cli::try_parse_from(["grava", "--print-default-config"])
                .unwrap()
                .print_default_config
        );
        assert!(
            Cli::try_parse_from(["grava", "--check-config"])
                .unwrap()
                .check_config
        );
        let init = Cli::try_parse_from(["grava", "--init", "--force"]).unwrap();
        assert!(init.init && init.force);
        assert!(Cli::try_parse_from(["grava", "--man"]).unwrap().man);
        for (name, shell) in [
            ("bash", CompletionShell::Bash),
            ("zsh", CompletionShell::Zsh),
            ("fish", CompletionShell::Fish),
        ] {
            assert_eq!(
                Cli::try_parse_from(["grava", "--completions", name])
                    .unwrap()
                    .completions,
                Some(shell)
            );
        }
    }

    #[test]
    fn rejects_invalid_option_values() {
        for (flag, value) in [
            ("--input", "microphone"),
            ("--shape", "triangle"),
            ("--glyphs", "sextant"),
            ("--particles", "0"),
            ("--particles", "many"),
            ("--particles", "1.5"),
            ("--fps", "0"),
            ("--fps", "sixty"),
            ("--seed", "invalid"),
            ("--completions", "powershell"),
        ] {
            assert!(
                Cli::try_parse_from(["grava", flag, value]).is_err(),
                "accepted {flag} {value}"
            );
        }
        assert!(Cli::try_parse_from(["grava", "--headless", "--frames", "0"]).is_err());
    }

    #[test]
    fn rejects_invalid_terminal_dimensions() {
        for value in [
            "", "80", "80X40", "80x40x20", "0x40", "80x0", "65536x40", "axb",
        ] {
            assert!(value.parse::<TerminalSize>().is_err(), "accepted {value}");
        }
        let size = "80x40".parse::<TerminalSize>().unwrap();
        assert_eq!(size.to_string(), "80x40");
        assert!(
            Cli::try_parse_from(["grava", "--headless", "--frames", "1", "--size", "0x40",])
                .is_err()
        );
    }

    #[test]
    fn enforces_action_conflicts_and_required_flags() {
        for arguments in [
            vec!["grava", "--check-config", "--print-default-config"],
            vec!["grava", "--init", "--list-themes"],
            vec!["grava", "--man", "--completions", "bash"],
            vec!["grava", "--force"],
            vec!["grava", "--headless"],
            vec!["grava", "--frames", "5"],
            vec!["grava", "--size", "80x40"],
            vec!["grava", "--headless", "--frames", "1", "--check-config"],
        ] {
            assert!(
                Cli::try_parse_from(&arguments).is_err(),
                "accepted {arguments:?}"
            );
        }
    }

    #[test]
    fn applies_only_explicit_configuration_overrides() {
        let mut config = Config::default();
        config.general.framerate = 30;
        config.input.source = "custom-source".to_owned();
        let previous = config.clone();
        Cli::try_parse_from(["grava"])
            .unwrap()
            .apply_overrides(&mut config);
        assert_eq!(config, previous);

        let cli = Cli::try_parse_from([
            "grava",
            "--input",
            "demo",
            "--source",
            "another-source",
            "--theme",
            "mono",
            "--shape",
            "circle",
            "--glyphs",
            "ascii",
            "--particles",
            "auto",
            "--fps",
            "75",
            "--no-color",
        ])
        .unwrap();
        cli.apply_overrides(&mut config);
        assert_eq!(config.input.method, InputMethod::Demo);
        assert_eq!(config.input.source, "another-source");
        assert_eq!(config.colors.theme, "mono");
        assert_eq!(config.plate.shape, Shape::Circle);
        assert_eq!(config.render.glyphs, Glyphs::Ascii);
        assert_eq!(config.particles.count, ParticleCount::Auto);
        assert_eq!(config.general.framerate, 75);
        assert_eq!(config.render.color_mode, ColorMode::None);
        assert_eq!(config.analysis, previous.analysis);
    }

    #[test]
    fn version_lists_enabled_audio_features_and_capture_status() {
        let error = Cli::try_parse_from(["grava", "--version"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayVersion);
        let version = error.to_string();
        assert!(version.contains(env!("CARGO_PKG_VERSION")));
        assert!(version.contains("enabled audio features:"));
        for (enabled, name) in [
            (cfg!(feature = "pulse"), "pulse"),
            (cfg!(feature = "pipewire"), "pipewire"),
            (cfg!(feature = "alsa"), "alsa"),
        ] {
            assert_eq!(version.contains(name), enabled);
        }
        assert!(version.contains("capture backends"));
    }
}
