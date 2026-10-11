//! Read configuration without creating files or mutating the process environment.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use super::{Config, ConfigError};

/// Resolved XDG locations. Paths may be absent when neither XDG nor HOME is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub config_file: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub explicit: bool,
}

impl ConfigPaths {
    /// Resolve the CLI path first, then XDG_CONFIG_HOME, then HOME/.config.
    /// Relative XDG paths are ignored, as required by the XDG specification.
    pub fn discover(explicit: Option<&Path>) -> Self {
        let xdg = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let home = env::var_os("HOME").map(PathBuf::from);
        Self::from_roots(explicit, xdg.as_deref(), home.as_deref())
    }

    /// Pure path resolution, also usable by embedders without environment access.
    pub fn from_roots(explicit: Option<&Path>, xdg: Option<&Path>, home: Option<&Path>) -> Self {
        if let Some(path) = explicit {
            return Self {
                config_file: Some(path.to_owned()),
                config_dir: path.parent().map(|parent| {
                    if parent.as_os_str().is_empty() {
                        PathBuf::from(".")
                    } else {
                        parent.to_owned()
                    }
                }),
                explicit: true,
            };
        }

        let config_dir = xdg
            .filter(|path| path.is_absolute())
            .map(|path| path.join("grava"))
            .or_else(|| {
                home.filter(|path| !path.as_os_str().is_empty())
                    .map(|path| path.join(".config/grava"))
            });
        Self {
            config_file: config_dir.as_ref().map(|dir| dir.join("config.toml")),
            config_dir,
            explicit: false,
        }
    }
}

/// Parsed settings and nonfatal unknown-key diagnostics.
#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub warnings: Vec<String>,
    /// The file actually read; `None` means embedded defaults were used.
    pub path: Option<PathBuf>,
}

/// Load an explicit file or an optional discovered file. Missing implicit files
/// use defaults; an explicitly requested missing file is an error.
pub fn load(paths: &ConfigPaths) -> Result<LoadedConfig, ConfigError> {
    load_with_overrides(paths, |_| {})
}

/// Apply CLI overrides before semantic validation so only effective values are
/// validated. TOML syntax and value types must still be valid in the source file.
pub fn load_with_overrides(
    paths: &ConfigPaths,
    overrides: impl FnOnce(&mut Config),
) -> Result<LoadedConfig, ConfigError> {
    let Some(path) = &paths.config_file else {
        return parse_with_overrides(super::DEFAULT_CONFIG, overrides);
    };
    match fs::read_to_string(path) {
        Ok(text) => {
            let mut loaded =
                parse_with_overrides(&text, overrides).map_err(|source| ConfigError::File {
                    path: path.clone(),
                    source: Box::new(source),
                })?;
            loaded.path = Some(path.clone());
            Ok(loaded)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound && !paths.explicit => {
            parse_with_overrides(super::DEFAULT_CONFIG, overrides)
        }
        Err(source) => Err(ConfigError::Read {
            path: path.clone(),
            source,
        }),
    }
}

/// Parse partial TOML over the Rust defaults, warn on unknown keys, and validate.
pub fn parse(text: &str) -> Result<LoadedConfig, ConfigError> {
    parse_with_overrides(text, |_| {})
}

fn parse_with_overrides(
    text: &str,
    overrides: impl FnOnce(&mut Config),
) -> Result<LoadedConfig, ConfigError> {
    let value: toml::Value = toml::from_str(text)?;
    let warnings = unknown_keys(&value);
    let mut config: Config = toml::from_str(text)?;
    overrides(&mut config);
    config.validate()?;
    Ok(LoadedConfig {
        config,
        warnings,
        path: None,
    })
}

// Explicit keys include optional theme overrides, which TOML serialization omits
// from Config::default(). Dynamic mapping.notes entries are validated by schema.
const SECTIONS: &[(&str, &[&str])] = &[
    (
        "general",
        &[
            "framerate",
            "autosens",
            "sensitivity",
            "sleep_timer",
            "live_reload",
        ],
    ),
    (
        "input",
        &[
            "method",
            "source",
            "channel_mix",
            "sample_rate",
            "sample_format",
            "channels",
        ],
    ),
    (
        "analysis",
        &[
            "fft_size",
            "low_cutoff",
            "high_cutoff",
            "noise_gate",
            "attack_ms",
            "release_ms",
        ],
    ),
    (
        "plate",
        &["shape", "edge", "symmetry", "max_order", "rotate_speed"],
    ),
    (
        "mapping",
        &[
            "mode",
            "octave_complexity",
            "base_octave",
            "min_confidence",
            "hold_ms",
            "transition_ms",
            "peaks",
            "notes",
        ],
    ),
    (
        "particles",
        &[
            "count", "density", "step", "jitter", "settle", "inertia", "kick",
        ],
    ),
    (
        "render",
        &[
            "glyphs",
            "ascii_ramp",
            "dot_threshold",
            "trail",
            "color_mode",
            "gradient_steps",
            "synchronized_output",
            "cell_aspect",
        ],
    ),
    ("layout", &["fit", "padding", "align"]),
    ("border", &["style", "title", "title_align"]),
    ("hud", &["enabled", "position", "format"]),
    (
        "colors",
        &[
            "theme",
            "pywal_path",
            "background",
            "sand",
            "colorize",
            "gradient",
            "note_colors",
            "border",
            "title",
            "hud_text",
            "hud_accent",
            "hud_dim",
            "hud_error",
        ],
    ),
];

fn unknown_keys(value: &toml::Value) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(table) = value.as_table() {
        for (section, value) in table {
            match SECTIONS.iter().find(|(name, _)| *name == section) {
                None => warnings.push(format!("unknown configuration key `{section}`")),
                Some((_, known)) => {
                    if let Some(table) = value.as_table() {
                        for key in table.keys().filter(|key| !known.contains(&key.as_str())) {
                            warnings.push(format!("unknown configuration key `{section}.{key}`"));
                        }
                    }
                }
            }
        }
    }
    warnings
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn path_precedence_is_explicit_then_xdg_then_home() {
        let explicit = ConfigPaths::from_roots(
            Some(Path::new("custom.toml")),
            Some(Path::new("/xdg")),
            Some(Path::new("/home/test")),
        );
        assert_eq!(explicit.config_file, Some(PathBuf::from("custom.toml")));
        assert_eq!(explicit.config_dir, Some(PathBuf::from(".")));
        assert!(explicit.explicit);
        let xdg =
            ConfigPaths::from_roots(None, Some(Path::new("/xdg")), Some(Path::new("/home/test")));
        assert_eq!(
            xdg.config_file,
            Some(PathBuf::from("/xdg/grava/config.toml"))
        );
        let home = ConfigPaths::from_roots(None, None, Some(Path::new("/home/test")));
        assert_eq!(
            home.config_file,
            Some(PathBuf::from("/home/test/.config/grava/config.toml"))
        );
    }

    #[test]
    fn relative_or_empty_xdg_uses_home_and_no_environment_uses_defaults() {
        for xdg in [Path::new(""), Path::new("relative")] {
            let paths = ConfigPaths::from_roots(None, Some(xdg), Some(Path::new("/home/test")));
            assert_eq!(
                paths.config_dir,
                Some(PathBuf::from("/home/test/.config/grava"))
            );
        }
        let paths = ConfigPaths::from_roots(None, None, None);
        assert!(paths.config_file.is_none());
        assert_eq!(load(&paths).unwrap().config, Config::default());
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let loaded = parse("[general]\nframerate = 90\n").unwrap();
        let mut expected = Config::default();
        expected.general.framerate = 90;
        assert_eq!(loaded.config, expected);
    }

    #[test]
    fn unknown_keys_warn_without_rejecting_the_config() {
        let loaded =
            parse("typo = 1\n[general]\nframrate = 90\n[mapping.notes]\nA = [2, 5]\n").unwrap();
        assert_eq!(
            loaded.warnings,
            vec![
                "unknown configuration key `general.framrate`",
                "unknown configuration key `typo`"
            ]
        );
        assert_eq!(loaded.config.general.framerate, 60);
    }

    #[test]
    fn all_serialized_schema_keys_are_recognized() {
        let serialized = toml::to_string(&Config::default()).unwrap();
        assert!(parse(&serialized).unwrap().warnings.is_empty());
        assert!(
            parse(super::super::DEFAULT_CONFIG)
                .unwrap()
                .warnings
                .is_empty()
        );
    }

    #[test]
    fn invalid_settings_include_the_key() {
        let error = parse("[analysis]\nfft_size = 3000\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("analysis.fft_size"), "{error}");
        let error = parse("[input]\nmethod = 'microphone'\n")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("method") && error.contains("pulse"),
            "{error}"
        );
    }
}
