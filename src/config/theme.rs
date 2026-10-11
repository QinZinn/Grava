//! Color themes: embedded built-ins, user theme files, and pywal.
//!
//! Layering, lowest first: the built-in `default` theme, the chosen theme,
//! the `[colors]` keys of the configuration. (Command-line flags sit above
//! all of these and are applied to the configuration before it gets here.)

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use super::{
    ConfigPaths,
    color::Color,
    schema::{Colorize, ColorsConfig},
};

/// The theme name that reads pywal's cache.
pub const PYWAL: &str = "pywal";

/// The built-in themes, in listing order.
pub const BUILTIN: [(&str, &str); 8] = [
    ("default", include_str!("../../assets/themes/default.toml")),
    ("sand", include_str!("../../assets/themes/sand.toml")),
    ("mono", include_str!("../../assets/themes/mono.toml")),
    (
        "catppuccin-mocha",
        include_str!("../../assets/themes/catppuccin-mocha.toml"),
    ),
    (
        "gruvbox-dark",
        include_str!("../../assets/themes/gruvbox-dark.toml"),
    ),
    ("nord", include_str!("../../assets/themes/nord.toml")),
    (
        "tokyo-night",
        include_str!("../../assets/themes/tokyo-night.toml"),
    ),
    (
        "rose-pine",
        include_str!("../../assets/themes/rose-pine.toml"),
    ),
];

/// The theme `--init` writes next to the configuration.
pub const SAMPLE_THEME: (&str, &str) = ("dune", include_str!("../../assets/sample/dune.toml"));

/// Every color of the interface, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorScheme {
    pub background: Color,
    pub sand: Color,
    pub colorize: Colorize,
    pub gradient: Vec<Color>,
    pub note_colors: [Color; 12],
    pub border: Color,
    pub title: Color,
    pub hud_text: Color,
    pub hud_accent: Color,
    pub hud_dim: Color,
    pub hud_error: Color,
}

/// A theme file: any key may be left out.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct ThemeFile {
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

const THEME_KEYS: [&str; 11] = [
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
];

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("unknown theme `{name}`; available: {available}")]
    Unknown { name: String, available: String },
    #[error("cannot read theme {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid theme {name}: {message}")]
    Invalid { name: String, message: String },
    #[error("pywal colors not found at {path}: run `wal -i IMAGE` first, or set colors.pywal_path")]
    PywalMissing { path: PathBuf },
}

/// Parse a theme and report keys that are not theme keys.
pub fn parse_theme(name: &str, text: &str) -> Result<(ThemeFile, Vec<String>), ThemeError> {
    let invalid = |message: String| ThemeError::Invalid {
        name: name.to_owned(),
        message,
    };
    let value: toml::Value = toml::from_str(text).map_err(|error| invalid(error.to_string()))?;
    let warnings = value
        .as_table()
        .into_iter()
        .flat_map(|table| table.keys())
        .filter(|key| !THEME_KEYS.contains(&key.as_str()))
        .map(|key| format!("unknown key `{key}` in theme {name}"))
        .collect();
    let file: ThemeFile = toml::from_str(text).map_err(|error| invalid(error.to_string()))?;
    if file.gradient.as_ref().is_some_and(Vec::is_empty) {
        return Err(invalid(
            "gradient must contain at least one color".to_owned(),
        ));
    }
    Ok((file, warnings))
}

impl ColorScheme {
    /// The built-in `default` theme: the terminal's own palette only.
    pub fn builtin_default() -> Self {
        let text = BUILTIN[0].1;
        let mut scheme = Self {
            background: Color::Default,
            sand: Color::Default,
            colorize: Colorize::Density,
            gradient: vec![Color::Default],
            note_colors: [Color::Default; 12],
            border: Color::Default,
            title: Color::Default,
            hud_text: Color::Default,
            hud_accent: Color::Default,
            hud_dim: Color::Default,
            hud_error: Color::Default,
        };
        match parse_theme("default", text) {
            Ok((theme, _)) => scheme.apply_theme(theme),
            // The theme is embedded and covered by a test.
            Err(_) => unreachable!("the embedded default theme is valid"),
        }
        scheme
    }

    /// Overlay the keys a theme sets.
    pub fn apply_theme(&mut self, theme: ThemeFile) {
        macro_rules! take {
            ($($field:ident),*) => {
                $(if let Some(value) = theme.$field { self.$field = value; })*
            };
        }
        take!(
            background,
            sand,
            colorize,
            gradient,
            note_colors,
            border,
            title,
            hud_text,
            hud_accent,
            hud_dim,
            hud_error
        );
    }

    /// Overlay the `[colors]` keys of the configuration, the last word.
    pub fn apply_overrides(&mut self, colors: &ColorsConfig) {
        macro_rules! take {
            ($($field:ident),*) => {
                $(if let Some(value) = &colors.$field { self.$field = value.clone(); })*
            };
        }
        take!(
            background,
            sand,
            colorize,
            gradient,
            note_colors,
            border,
            title,
            hud_text,
            hud_accent,
            hud_dim,
            hud_error
        );
    }
}

/// Where themes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeLocations {
    /// `<config dir>/themes`, if there is a config directory at all.
    pub directory: Option<PathBuf>,
    /// pywal's `colors.json`.
    pub pywal: PathBuf,
}

impl ThemeLocations {
    pub fn new(paths: &ConfigPaths, colors: &ColorsConfig) -> Self {
        let cache = env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".cache")));
        Self {
            directory: paths.config_dir.as_ref().map(|dir| dir.join("themes")),
            pywal: colors.pywal_path.clone().unwrap_or_else(|| {
                cache
                    .unwrap_or_else(|| PathBuf::from(".cache"))
                    .join("wal/colors.json")
            }),
        }
    }

    /// The user's file for theme `name`, if one exists.
    fn user_file(&self, name: &str) -> Option<PathBuf> {
        // Names are plain words; anything that could climb out of the
        // directory is not a name (paths are handled before this).
        if name.is_empty() || name.contains(['/', '\\', '\0']) || name.starts_with('.') {
            return None;
        }
        let path = self.directory.as_ref()?.join(format!("{name}.toml"));
        path.is_file().then_some(path)
    }
}

/// Where a resolved theme came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeSource {
    Builtin,
    File(PathBuf),
    Pywal(PathBuf),
}

#[derive(Debug)]
pub struct ResolvedTheme {
    pub scheme: ColorScheme,
    pub source: ThemeSource,
    pub warnings: Vec<String>,
}

impl ResolvedTheme {
    /// The file whose change should reload the colors.
    pub fn watched_file(&self) -> Option<&Path> {
        match &self.source {
            ThemeSource::Builtin => None,
            ThemeSource::File(path) | ThemeSource::Pywal(path) => Some(path),
        }
    }
}

/// Resolve `colors.theme` and layer the `[colors]` overrides on top.
pub fn resolve(
    colors: &ColorsConfig,
    locations: &ThemeLocations,
) -> Result<ResolvedTheme, ThemeError> {
    let name = colors.theme.as_str();
    let mut scheme = ColorScheme::builtin_default();
    let (source, warnings) = if name == PYWAL {
        let theme = pywal_theme(&locations.pywal)?;
        scheme.apply_theme(theme);
        (ThemeSource::Pywal(locations.pywal.clone()), Vec::new())
    } else if name.contains('/') {
        // An explicit path to a theme file.
        let path = PathBuf::from(name);
        let (theme, warnings) = read_theme_file(name, &path)?;
        scheme.apply_theme(theme);
        (ThemeSource::File(path), warnings)
    } else if let Some(path) = locations.user_file(name) {
        // A file of the same name beats the built-in theme.
        let (theme, warnings) = read_theme_file(name, &path)?;
        scheme.apply_theme(theme);
        (ThemeSource::File(path), warnings)
    } else if let Some((_, text)) = BUILTIN.iter().find(|(builtin, _)| *builtin == name) {
        let (theme, warnings) = parse_theme(name, text)?;
        scheme.apply_theme(theme);
        (ThemeSource::Builtin, warnings)
    } else {
        return Err(ThemeError::Unknown {
            name: name.to_owned(),
            available: names(&list(locations))
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        });
    };
    scheme.apply_overrides(colors);
    Ok(ResolvedTheme {
        scheme,
        source,
        warnings,
    })
}

fn read_theme_file(name: &str, path: &Path) -> Result<(ThemeFile, Vec<String>), ThemeError> {
    let text = fs::read_to_string(path).map_err(|source| ThemeError::Read {
        path: path.to_owned(),
        source,
    })?;
    parse_theme(name, &text)
}

/// The shape of pywal's `colors.json` that matters here.
#[derive(Deserialize)]
struct Wal {
    special: WalSpecial,
    colors: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct WalSpecial {
    foreground: String,
}

/// Map pywal's palette onto a theme:
///
/// * `sand` and `hud_text`: the foreground,
/// * `gradient`: color1, color4, color6, then the foreground,
/// * `border` and `hud_dim`: color8,
/// * `title` and `hud_accent`: color4,
/// * `hud_error`: color1,
/// * `note_colors`: colors 1-6 and 9-14, red to magenta around the wheel,
/// * the background is left to the terminal, which wal already themed.
fn pywal_theme(path: &Path) -> Result<ThemeFile, ThemeError> {
    let text = fs::read_to_string(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ThemeError::PywalMissing {
                path: path.to_owned(),
            }
        } else {
            ThemeError::Read {
                path: path.to_owned(),
                source,
            }
        }
    })?;
    pywal_from_json(&text, path)
}

fn pywal_from_json(text: &str, path: &Path) -> Result<ThemeFile, ThemeError> {
    let invalid = |message: String| ThemeError::Invalid {
        name: format!("pywal ({})", path.display()),
        message,
    };
    let wal: Wal = serde_json::from_str(text).map_err(|error| invalid(error.to_string()))?;
    let color = |value: &str| {
        value
            .parse::<Color>()
            .map_err(|error| invalid(error.to_string()))
    };
    let slot = |index: u8| {
        let key = format!("color{index}");
        wal.colors
            .get(&key)
            .ok_or_else(|| invalid(format!("missing {key}")))
            .and_then(|value| color(value))
    };
    let foreground = color(&wal.special.foreground)?;
    let accent = slot(4)?;
    let wheel = [1, 9, 3, 11, 2, 10, 6, 14, 4, 12, 5, 13];
    let mut note_colors = [Color::Default; 12];
    for (target, index) in note_colors.iter_mut().zip(wheel) {
        *target = slot(index)?;
    }
    Ok(ThemeFile {
        background: Some(Color::Default),
        sand: Some(foreground),
        colorize: Some(Colorize::Density),
        gradient: Some(vec![slot(1)?, accent, slot(6)?, foreground]),
        note_colors: Some(note_colors),
        border: Some(slot(8)?),
        title: Some(accent),
        hud_text: Some(foreground),
        hud_accent: Some(accent),
        hud_dim: Some(slot(8)?),
        hud_error: Some(slot(1)?),
    })
}

/// A theme that can be chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeEntry {
    pub name: String,
    pub kind: ThemeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeKind {
    Builtin,
    /// A file in the themes directory; `overrides` when a built-in has its name.
    File {
        path: PathBuf,
        overrides: bool,
    },
    Pywal {
        path: PathBuf,
        available: bool,
    },
}

/// Built-in themes, then the user's own (sorted), then pywal.
pub fn list(locations: &ThemeLocations) -> Vec<ThemeEntry> {
    let mut entries: Vec<ThemeEntry> = BUILTIN
        .iter()
        .map(|(name, _)| ThemeEntry {
            name: (*name).to_owned(),
            kind: ThemeKind::Builtin,
        })
        .collect();
    let mut files: Vec<(String, PathBuf)> = locations
        .directory
        .as_ref()
        .and_then(|directory| fs::read_dir(directory).ok())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml") && path.is_file())
        .filter_map(|path| {
            let name = path.file_stem()?.to_str()?.to_owned();
            (!name.starts_with('.')).then_some((name, path))
        })
        .collect();
    files.sort();
    for (name, path) in files {
        if let Some(existing) = entries.iter_mut().find(|entry| entry.name == name) {
            existing.kind = ThemeKind::File {
                path,
                overrides: true,
            };
        } else {
            entries.push(ThemeEntry {
                name,
                kind: ThemeKind::File {
                    path,
                    overrides: false,
                },
            });
        }
    }
    entries.push(ThemeEntry {
        name: PYWAL.to_owned(),
        kind: ThemeKind::Pywal {
            available: locations.pywal.is_file(),
            path: locations.pywal.clone(),
        },
    });
    entries
}

fn names(entries: &[ThemeEntry]) -> Vec<String> {
    entries.iter().map(|entry| entry.name.clone()).collect()
}

/// The theme after (`forward`) or before `current` in the cycle of the `t`
/// and `T` keys. Pywal joins the cycle only when its cache exists.
pub fn cycle(current: &str, locations: &ThemeLocations, forward: bool) -> String {
    let entries: Vec<String> = list(locations)
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.kind,
                ThemeKind::Pywal {
                    available: false,
                    ..
                }
            )
        })
        .map(|entry| entry.name)
        .collect();
    let position = entries.iter().position(|name| name == current);
    let count = entries.len();
    let next = match (position, forward) {
        (Some(index), true) => (index + 1) % count,
        (Some(index), false) => (index + count - 1) % count,
        (None, true) => 0,
        (None, false) => count - 1,
    };
    entries[next].clone()
}

/// The table printed by `--list-themes`; `*` marks the active theme.
pub fn format_list(entries: &[ThemeEntry], active: &str) -> String {
    let width = entries
        .iter()
        .map(|entry| entry.name.len())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for entry in entries {
        let marker = if entry.name == active { '*' } else { ' ' };
        let origin = match &entry.kind {
            ThemeKind::Builtin => "built-in".to_owned(),
            ThemeKind::File {
                path,
                overrides: false,
            } => path.display().to_string(),
            ThemeKind::File {
                path,
                overrides: true,
            } => format!("{} (replaces the built-in theme)", path.display()),
            ThemeKind::Pywal { path, available } => {
                if *available {
                    path.display().to_string()
                } else {
                    format!("{} (not found)", path.display())
                }
            }
        };
        text.push_str(&format!("{marker} {:<width$}  {origin}\n", entry.name));
    }
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::color::AnsiColor;

    struct Directory(PathBuf);

    impl Directory {
        fn new(label: &str) -> Self {
            let path = env::temp_dir().join(format!("grava-theme-{}-{label}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join("themes")).unwrap();
            Self(path)
        }

        fn locations(&self) -> ThemeLocations {
            ThemeLocations {
                directory: Some(self.0.join("themes")),
                pywal: self.0.join("wal/colors.json"),
            }
        }

        fn theme(&self, name: &str, text: &str) -> PathBuf {
            let path = self.0.join("themes").join(format!("{name}.toml"));
            fs::write(&path, text).unwrap();
            path
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn rgb(hex: &str) -> Color {
        hex.parse().unwrap()
    }

    fn colors(theme: &str) -> ColorsConfig {
        ColorsConfig {
            theme: theme.to_owned(),
            ..ColorsConfig::default()
        }
    }

    #[test]
    fn every_builtin_theme_sets_every_key_and_parses_without_warnings() {
        assert_eq!(BUILTIN.len(), 8);
        for (name, text) in BUILTIN.iter().chain(&[SAMPLE_THEME]) {
            let (theme, warnings) = parse_theme(name, text).unwrap();
            assert!(warnings.is_empty(), "{name}: {warnings:?}");
            assert!(theme.background.is_some() && theme.sand.is_some(), "{name}");
            assert!(theme.colorize.is_some(), "{name}");
            assert!(theme.border.is_some() && theme.title.is_some(), "{name}");
            assert!(
                theme.hud_text.is_some() && theme.hud_accent.is_some(),
                "{name}"
            );
            assert!(
                theme.hud_dim.is_some() && theme.hud_error.is_some(),
                "{name}"
            );
            if *name != SAMPLE_THEME.0 {
                assert_eq!(theme.gradient.as_ref().unwrap().len(), 4, "{name}");
                assert!(theme.note_colors.is_some(), "{name}");
            }
        }
        let names: Vec<_> = BUILTIN.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            [
                "default",
                "sand",
                "mono",
                "catppuccin-mocha",
                "gruvbox-dark",
                "nord",
                "tokyo-night",
                "rose-pine"
            ]
        );
    }

    #[test]
    fn the_default_theme_uses_only_the_terminal_palette() {
        let scheme = ColorScheme::builtin_default();
        let palette_only = |color: &Color| !matches!(color, Color::Rgb { .. });
        assert!(scheme.gradient.iter().all(palette_only));
        assert!(scheme.note_colors.iter().all(palette_only));
        for color in [
            scheme.background,
            scheme.sand,
            scheme.border,
            scheme.title,
            scheme.hud_text,
            scheme.hud_accent,
            scheme.hud_dim,
            scheme.hud_error,
        ] {
            assert!(palette_only(&color), "{color}");
        }
        assert_eq!(scheme.colorize, Colorize::Density);
        assert_eq!(
            scheme.gradient,
            [
                Color::Indexed(8),
                Color::Indexed(4),
                Color::Indexed(6),
                Color::Indexed(15)
            ]
        );
        // Official palette values, spot-checked.
        let (theme, _) = parse_theme("c", BUILTIN[3].1).unwrap();
        assert_eq!(theme.note_colors.unwrap()[0], rgb("#f38ba8"));
        let (theme, _) = parse_theme("n", BUILTIN[5].1).unwrap();
        assert_eq!(theme.hud_error, Some(rgb("#bf616a")));
        let (theme, _) = parse_theme("t", BUILTIN[6].1).unwrap();
        assert_eq!(theme.hud_accent, Some(rgb("#7aa2f7")));
        let (theme, _) = parse_theme("g", BUILTIN[4].1).unwrap();
        assert_eq!(theme.hud_error, Some(rgb("#fb4934")));
        let (theme, _) = parse_theme("r", BUILTIN[7].1).unwrap();
        assert_eq!(theme.hud_error, Some(rgb("#eb6f92")));
    }

    #[test]
    fn layering_is_default_then_theme_then_config_overrides() {
        let directory = Directory::new("layers");
        let locations = directory.locations();
        let nord = resolve(&colors("nord"), &locations).unwrap();
        assert_eq!(nord.source, ThemeSource::Builtin);
        assert_eq!(nord.scheme.sand, rgb("#d8dee9"));
        // A theme that sets only one key keeps the rest of the default theme.
        directory.theme("tiny", "sand = \"#010203\"\n");
        let tiny = resolve(&colors("tiny"), &locations).unwrap();
        assert_eq!(tiny.scheme.sand, rgb("#010203"));
        assert_eq!(
            tiny.scheme.gradient,
            ColorScheme::builtin_default().gradient
        );
        assert!(matches!(tiny.source, ThemeSource::File(_)));
        assert!(tiny.watched_file().is_some());
        // [colors] keys beat the theme.
        let mut config = colors("tiny");
        config.sand = Some(rgb("#ffffff"));
        config.colorize = Some(Colorize::Radial);
        config.note_colors = Some([Color::Ansi(AnsiColor::Red); 12]);
        let overridden = resolve(&config, &locations).unwrap();
        assert_eq!(overridden.scheme.sand, rgb("#ffffff"));
        assert_eq!(overridden.scheme.colorize, Colorize::Radial);
        assert_eq!(
            overridden.scheme.note_colors[5],
            Color::Ansi(AnsiColor::Red)
        );
        assert!(
            resolve(&colors("nord"), &locations)
                .unwrap()
                .watched_file()
                .is_none()
        );
    }

    #[test]
    fn a_user_file_replaces_the_builtin_theme_of_the_same_name() {
        let directory = Directory::new("replace");
        let locations = directory.locations();
        directory.theme("nord", "sand = \"#123456\"\nhud_error = \"#654321\"\n");
        let theme = resolve(&colors("nord"), &locations).unwrap();
        assert_eq!(theme.scheme.sand, rgb("#123456"));
        // The user's file is a full replacement, not a patch of the built-in.
        assert_eq!(theme.scheme.border, ColorScheme::builtin_default().border);
        let entries = list(&locations);
        let nord = entries.iter().find(|entry| entry.name == "nord").unwrap();
        assert!(matches!(
            nord.kind,
            ThemeKind::File {
                overrides: true,
                ..
            }
        ));
        assert_eq!(
            entries.iter().filter(|entry| entry.name == "nord").count(),
            1
        );
    }

    #[test]
    fn unknown_themes_and_broken_files_are_explained() {
        let directory = Directory::new("errors");
        let locations = directory.locations();
        directory.theme("mine", "gradient = []\n");
        let error = resolve(&colors("nope"), &locations)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown theme `nope`"), "{error}");
        for expected in ["default", "rose-pine", "mine", "pywal"] {
            assert!(error.contains(expected), "{error}");
        }
        let error = resolve(&colors("mine"), &locations)
            .unwrap_err()
            .to_string();
        assert!(error.contains("gradient"), "{error}");
        directory.theme("bad", "sand = \"purple\"\n");
        let error = resolve(&colors("bad"), &locations).unwrap_err().to_string();
        assert!(
            error.contains("bad") && error.contains("#rrggbb"),
            "{error}"
        );
        directory.theme("typo", "snad = \"#000000\"\nsand = \"#111111\"\n");
        let theme = resolve(&colors("typo"), &locations).unwrap();
        assert_eq!(theme.warnings, ["unknown key `snad` in theme typo"]);
        // Names cannot climb out of the themes directory.
        for name in ["../escape", "a\\b", ".hidden"] {
            assert!(resolve(&colors(name), &locations).is_err(), "{name}");
        }
    }

    #[test]
    fn a_path_names_a_theme_file_directly() {
        let directory = Directory::new("path");
        let path = directory.0.join("elsewhere.toml");
        fs::write(&path, "sand = \"#abcdef\"\n").unwrap();
        let theme = resolve(&colors(path.to_str().unwrap()), &directory.locations()).unwrap();
        assert_eq!(theme.scheme.sand, rgb("#abcdef"));
        assert_eq!(theme.source, ThemeSource::File(path));
        let missing = resolve(&colors("/does/not/exist.toml"), &directory.locations());
        assert!(
            missing
                .unwrap_err()
                .to_string()
                .contains("cannot read theme")
        );
    }

    const WAL: &str = r##"{
        "wallpaper": "/tmp/x.png", "alpha": "100",
        "special": {"background": "#101010", "foreground": "#eeeeee", "cursor": "#eeeeee"},
        "colors": {
            "color0": "#101010", "color1": "#aa0001", "color2": "#aa0002", "color3": "#aa0003",
            "color4": "#aa0004", "color5": "#aa0005", "color6": "#aa0006", "color7": "#aa0007",
            "color8": "#bb0008", "color9": "#bb0009", "color10": "#bb000a", "color11": "#bb000b",
            "color12": "#bb000c", "color13": "#bb000d", "color14": "#bb000e", "color15": "#bb000f"
        }
    }"##;

    #[test]
    fn pywal_maps_its_palette_as_documented() {
        let directory = Directory::new("wal");
        fs::create_dir_all(directory.0.join("wal")).unwrap();
        fs::write(directory.0.join("wal/colors.json"), WAL).unwrap();
        let theme = resolve(&colors(PYWAL), &directory.locations()).unwrap();
        let scheme = theme.scheme.clone();
        assert_eq!(scheme.sand, rgb("#eeeeee"));
        assert_eq!(scheme.hud_text, rgb("#eeeeee"));
        assert_eq!(
            scheme.gradient,
            [
                rgb("#aa0001"),
                rgb("#aa0004"),
                rgb("#aa0006"),
                rgb("#eeeeee")
            ]
        );
        assert_eq!(
            (scheme.border, scheme.hud_dim),
            (rgb("#bb0008"), rgb("#bb0008"))
        );
        assert_eq!(
            (scheme.title, scheme.hud_accent),
            (rgb("#aa0004"), rgb("#aa0004"))
        );
        assert_eq!(scheme.hud_error, rgb("#aa0001"));
        // The terminal keeps its own background, which wal already themed.
        assert_eq!(scheme.background, Color::Default);
        assert_eq!(scheme.note_colors[0], rgb("#aa0001"));
        assert_eq!(scheme.note_colors[1], rgb("#bb0009"));
        assert_eq!(scheme.note_colors[11], rgb("#bb000d"));
        assert!(matches!(theme.source, ThemeSource::Pywal(_)));
        assert!(theme.watched_file().unwrap().ends_with("colors.json"));
    }

    #[test]
    fn pywal_problems_say_what_to_do() {
        let directory = Directory::new("wal-errors");
        let locations = directory.locations();
        let missing = resolve(&colors(PYWAL), &locations).unwrap_err().to_string();
        assert!(
            missing.contains("wal -i") && missing.contains("pywal_path"),
            "{missing}"
        );
        let path = Path::new("colors.json");
        for (json, expected) in [
            ("not json", "pywal"),
            (
                r##"{"special":{"foreground":"#fff"},"colors":{}}"##,
                "pywal",
            ),
            (
                r##"{"special":{"foreground":"#ffffff"},"colors":{"color1":"#000000"}}"##,
                "missing color4",
            ),
        ] {
            let error = pywal_from_json(json, path).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
        }
        // The cache directory comes from the configuration when it says so.
        let configured = ColorsConfig {
            pywal_path: Some(PathBuf::from("/tmp/elsewhere.json")),
            ..ColorsConfig::default()
        };
        let paths = ConfigPaths::from_roots(None, Some(Path::new("/xdg")), None);
        assert_eq!(
            ThemeLocations::new(&paths, &configured).pywal,
            PathBuf::from("/tmp/elsewhere.json")
        );
        assert_eq!(
            ThemeLocations::new(&paths, &configured).directory,
            Some(PathBuf::from("/xdg/grava/themes"))
        );
    }

    #[test]
    fn listing_and_cycling_cover_builtin_user_and_available_pywal_themes() {
        let directory = Directory::new("cycle");
        let locations = directory.locations();
        directory.theme("zeta", "");
        directory.theme("alpha", "");
        fs::write(directory.0.join("themes/notes.txt"), "").unwrap();
        let entries = list(&locations);
        let order: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(&order[8..], ["alpha", "zeta", "pywal"]);
        // Without a wal cache pywal is listed but not part of the cycle.
        assert_eq!(cycle("zeta", &locations, true), "default");
        assert_eq!(cycle("default", &locations, false), "zeta");
        assert_eq!(cycle("default", &locations, true), "sand");
        assert_eq!(cycle("not-a-theme", &locations, true), "default");
        assert_eq!(cycle("not-a-theme", &locations, false), "zeta");
        fs::create_dir_all(directory.0.join("wal")).unwrap();
        fs::write(directory.0.join("wal/colors.json"), WAL).unwrap();
        assert_eq!(cycle("zeta", &locations, true), "pywal");
        assert_eq!(cycle("pywal", &locations, true), "default");
        // Walking the whole cycle visits every entry exactly once.
        let mut seen = vec!["default".to_owned()];
        loop {
            let next = cycle(seen.last().unwrap(), &locations, true);
            if next == "default" {
                break;
            }
            seen.push(next);
        }
        assert_eq!(seen.len(), 11);
    }

    #[test]
    fn the_listing_marks_the_active_theme_and_where_each_theme_lives() {
        let directory = Directory::new("format");
        let locations = directory.locations();
        directory.theme("nord", "");
        directory.theme("dune", "");
        let text = format_list(&list(&locations), "dune");
        let line = |name: &str| {
            text.lines()
                .find(|line| line[2..].starts_with(name))
                .unwrap()
                .to_owned()
        };
        assert!(line("dune").starts_with("* dune"));
        assert!(line("default").starts_with("  default") && line("default").ends_with("built-in"));
        assert!(line("nord").contains("replaces the built-in theme"));
        assert!(line("pywal").ends_with("(not found)"));
        assert!(line("dune").contains("themes/dune.toml"));
    }
}
