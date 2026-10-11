//! Configuration schema, explicit file loading, and validation.

pub mod color;
pub mod load;
pub mod schema;
pub mod template;
pub mod theme;
pub mod watch;

pub use load::{ConfigPaths, LoadedConfig, load, load_with_overrides, parse};
pub use schema::Config;

/// Fully documented defaults, also available through `--print-default-config`.
pub const DEFAULT_CONFIG: &str = include_str!("../../assets/config.toml");

/// Configuration errors retain the offending path or setting for diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid configuration {path}: {source}")]
    File {
        path: std::path::PathBuf,
        #[source]
        source: Box<ConfigError>,
    },
    #[error("cannot read configuration {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid TOML configuration: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid value for {key}: {message}")]
    Validation { key: String, message: String },
}
