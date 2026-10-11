//! Choosing a live backend, and listing the sources of the chosen one.
//!
//! `method = "auto"` tries pipewire, then pulse, then alsa, and uses the first
//! that connects. Backends that were not compiled in, or whose server is not
//! there, are skipped with a reason; if none works the error lists every
//! reason and what to do next.

use crate::config::schema::{InputConfig, InputMethod};

use super::{AudioError, Capture};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Pipewire,
    Pulse,
    Alsa,
}

impl Backend {
    /// The order in which `method = "auto"` tries the backends.
    pub const AUTO_ORDER: [Backend; 3] = [Backend::Pipewire, Backend::Pulse, Backend::Alsa];

    pub fn name(self) -> &'static str {
        match self {
            Self::Pipewire => "pipewire",
            Self::Pulse => "pulse",
            Self::Alsa => "alsa",
        }
    }

    /// Whether this build includes the backend.
    pub fn compiled(self) -> bool {
        match self {
            Self::Pipewire => cfg!(feature = "pipewire"),
            Self::Pulse => cfg!(feature = "pulse"),
            Self::Alsa => cfg!(feature = "alsa"),
        }
    }

    /// The backend a method names; `None` for `auto` and the non-live inputs.
    pub fn from_method(method: InputMethod) -> Option<Self> {
        match method {
            InputMethod::Pipewire => Some(Self::Pipewire),
            InputMethod::Pulse => Some(Self::Pulse),
            InputMethod::Alsa => Some(Self::Alsa),
            InputMethod::Auto | InputMethod::Fifo | InputMethod::Stdin | InputMethod::Demo => None,
        }
    }

    fn not_compiled(self) -> String {
        format!(
            "not compiled in (rebuild with `--features {}`)",
            self.name()
        )
    }
}

/// One capturable source, as printed by `--list-sources`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    /// The name to pass as `source`.
    pub name: String,
    pub description: String,
    /// A monitor of an output: what you hear rather than a microphone.
    pub monitor: bool,
    /// What `source = "auto"` captures.
    pub default: bool,
}

/// The backends to try for `method`, in order.
fn candidates(method: InputMethod) -> Vec<Backend> {
    match Backend::from_method(method) {
        Some(backend) => vec![backend],
        None => Backend::AUTO_ORDER.to_vec(),
    }
}

/// Run `attempt` on each candidate until one succeeds. An explicit method
/// reports its own failure; `auto` summarizes all of them.
fn first_working<T>(
    method: InputMethod,
    mut attempt: impl FnMut(Backend) -> Result<T, String>,
) -> Result<(Backend, T), AudioError> {
    let mut failures = Vec::new();
    for backend in candidates(method) {
        match attempt(backend) {
            Ok(value) => return Ok((backend, value)),
            Err(reason) if method != InputMethod::Auto => {
                return Err(AudioError::Unsupported(format!(
                    "{}: {reason}",
                    backend.name()
                )));
            }
            Err(reason) => failures.push((backend, reason)),
        }
    }
    Err(AudioError::Unsupported(no_backend_message(&failures)))
}

fn no_backend_message(failures: &[(Backend, String)]) -> String {
    let mut message = String::from("no audio backend could be opened:");
    for (backend, reason) in failures {
        message.push_str(&format!("\n  {}: {reason}", backend.name()));
    }
    message.push_str(
        "\nStart pipewire-pulse (or pulseaudio), rebuild with `--features pipewire` for native \
         PipeWire, or pipe audio in with `--input stdin` or `--input fifo`.",
    );
    message
}

/// Open the live capture for `config.method`: one backend, or the first that
/// works for `auto`. Connection problems surface here, before the terminal is
/// touched.
pub fn open_capture(config: &InputConfig) -> Result<Box<dyn Capture>, AudioError> {
    first_working(config.method, |backend| open_backend(backend, config))
        .map(|(_, capture)| capture)
}

#[allow(unused_variables)]
fn open_backend(backend: Backend, config: &InputConfig) -> Result<Box<dyn Capture>, String> {
    if !backend.compiled() {
        return Err(backend.not_compiled());
    }
    match backend {
        #[cfg(feature = "pipewire")]
        Backend::Pipewire => super::pipewire::PipewireCapture::open(config)
            .map(|capture| Box::new(capture) as Box<dyn Capture>)
            .map_err(|error| error.to_string()),
        #[cfg(feature = "pulse")]
        Backend::Pulse => super::pulse::PulseCapture::open(config)
            .map(|capture| Box::new(capture) as Box<dyn Capture>)
            .map_err(|error| error.to_string()),
        #[cfg(feature = "alsa")]
        Backend::Alsa => super::alsa::AlsaCapture::open(config)
            .map(|capture| Box::new(capture) as Box<dyn Capture>)
            .map_err(|error| error.to_string()),
        #[allow(unreachable_patterns)]
        _ => Err(backend.not_compiled()),
    }
}

/// The sources of the selected backend (the first working one for `auto`).
pub fn list_sources(config: &InputConfig) -> Result<(Backend, Vec<SourceEntry>), AudioError> {
    first_working(config.method, |backend| list_backend(backend, config))
}

#[allow(unused_variables)]
fn list_backend(backend: Backend, config: &InputConfig) -> Result<Vec<SourceEntry>, String> {
    if !backend.compiled() {
        return Err(backend.not_compiled());
    }
    match backend {
        #[cfg(feature = "pipewire")]
        Backend::Pipewire => super::pipewire::list_sources().map_err(|error| error.to_string()),
        #[cfg(feature = "pulse")]
        Backend::Pulse => super::pulse::list_sources().map_err(|error| error.to_string()),
        #[cfg(feature = "alsa")]
        Backend::Alsa => {
            let sources = super::alsa::list_sources().map_err(|error| error.to_string())?;
            // ALSA always lists generic devices, so `auto` settles on it only
            // when its default, a loopback capture device, is really there.
            if config.method == InputMethod::Auto && !sources.iter().any(|source| source.default) {
                return Err("no loopback capture device (`sudo modprobe snd-aloop`); \
                     name a device with `-i alsa` to use another"
                    .to_owned());
            }
            Ok(sources)
        }
        #[allow(unreachable_patterns)]
        _ => Err(backend.not_compiled()),
    }
}

/// The table printed by `--list-sources`: `*` marks what `source = "auto"`
/// captures, and the kind column tells monitors (outputs) from inputs.
pub fn format_sources(backend: Backend, entries: &[SourceEntry]) -> String {
    let width = entries
        .iter()
        .map(|entry| entry.name.chars().count())
        .max()
        .unwrap_or(0);
    let mut text = format!(
        "# {} sources; * is captured by source = \"auto\", monitors carry what you hear\n",
        backend.name()
    );
    for entry in entries {
        let marker = if entry.default { '*' } else { ' ' };
        let kind = if entry.monitor { "monitor" } else { "input" };
        text.push_str(&format!(
            "{marker} {:<width$}  {kind:<7}  {}\n",
            entry.name, entry.description
        ));
    }
    if entries.is_empty() {
        text.push_str("# (no sources found)\n");
    }
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn auto_tries_pipewire_then_pulse_then_alsa_and_stops_at_the_first_success() {
        let mut tried = Vec::new();
        let (backend, value) = first_working(InputMethod::Auto, |backend| {
            tried.push(backend);
            if backend == Backend::Pulse {
                Ok("connected")
            } else {
                Err("unavailable".to_owned())
            }
        })
        .unwrap();
        assert_eq!((backend, value), (Backend::Pulse, "connected"));
        assert_eq!(tried, [Backend::Pipewire, Backend::Pulse]);
    }

    #[test]
    fn auto_failure_lists_every_attempt_and_what_to_do_next() {
        let error = first_working(InputMethod::Auto, |backend| {
            Err::<(), _>(match backend {
                Backend::Pulse => "no PulseAudio socket".to_owned(),
                other => other.not_compiled(),
            })
        })
        .unwrap_err()
        .to_string();
        for expected in [
            "no audio backend could be opened",
            "pipewire: not compiled in (rebuild with `--features pipewire`)",
            "pulse: no PulseAudio socket",
            "alsa: not compiled in",
            "pipewire-pulse",
            "--input stdin",
            "fifo",
        ] {
            assert!(
                error.contains(expected),
                "{expected:?} missing from:\n{error}"
            );
        }
        // Attempts appear in the order they were made.
        assert!(error.find("pipewire:").unwrap() < error.find("pulse:").unwrap());
        assert!(error.find("pulse:").unwrap() < error.find("alsa:").unwrap());
    }

    #[test]
    fn an_explicit_method_tries_only_itself_and_names_the_backend() {
        let mut tried = Vec::new();
        let error = first_working(InputMethod::Alsa, |backend| {
            tried.push(backend);
            Err::<(), _>("device busy".to_owned())
        })
        .unwrap_err()
        .to_string();
        assert_eq!(tried, [Backend::Alsa]);
        assert_eq!(error, "alsa: device busy");
        assert_eq!(
            Backend::from_method(InputMethod::Pulse),
            Some(Backend::Pulse)
        );
        for method in [
            InputMethod::Auto,
            InputMethod::Demo,
            InputMethod::Stdin,
            InputMethod::Fifo,
        ] {
            assert_eq!(Backend::from_method(method), None);
        }
    }

    #[test]
    fn backends_missing_from_the_build_explain_the_feature_to_enable() {
        for backend in Backend::AUTO_ORDER {
            if backend.compiled() {
                continue;
            }
            let config = InputConfig {
                method: match backend {
                    Backend::Pipewire => InputMethod::Pipewire,
                    Backend::Pulse => InputMethod::Pulse,
                    Backend::Alsa => InputMethod::Alsa,
                },
                ..InputConfig::default()
            };
            let message = open_capture(&config).err().unwrap().to_string();
            assert!(
                message.contains(&format!("--features {}", backend.name())),
                "{message}"
            );
            assert!(list_sources(&config).is_err());
        }
    }

    #[test]
    fn the_source_table_marks_the_default_and_monitors_and_aligns_columns() {
        let entries = [
            SourceEntry {
                name: "alsa_input.mic".to_owned(),
                description: "Microphone".to_owned(),
                monitor: false,
                default: false,
            },
            SourceEntry {
                name: "alsa_output.speakers.monitor".to_owned(),
                description: "Monitor of Speakers".to_owned(),
                monitor: true,
                default: true,
            },
        ];
        let table = format_sources(Backend::Pulse, &entries);
        let lines: Vec<_> = table.lines().collect();
        assert!(lines[0].starts_with("# pulse sources"));
        assert_eq!(
            lines[1],
            format!("  {:<28}  {:<7}  Microphone", "alsa_input.mic", "input")
        );
        assert_eq!(
            lines[2],
            "* alsa_output.speakers.monitor  monitor  Monitor of Speakers"
        );
        assert!(format_sources(Backend::Alsa, &[]).contains("no sources found"));
    }
}
