//! Every key of the default configuration must do something you can see.
//!
//! The test walks the keys of the embedded default config. For each one it
//! renders a deterministic headless run twice: once with the key as shipped and
//! once with an alternative value, and requires the frames to differ. A key
//! that has no probe must appear in [`EXEMPT`] with the reason why it cannot be
//! seen in a frame, so a new key cannot be added without making that decision.

#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use grava::config::DEFAULT_CONFIG;

/// Where the audio of a probe comes from.
#[derive(Clone, Copy)]
#[repr(u8)]
enum Feed {
    /// The built-in demo synth, which is already analyzed like real audio.
    Demo,
    /// Stereo 16-bit PCM on stdin: 440 Hz on the left, 330 Hz on the right.
    Stereo,
    /// Half a second of tone, then silence: a plate that has been quiet for
    /// two seconds by the end of the run.
    ToneThenSilence,
}

struct Probe {
    key: &'static str,
    /// The alternative value, as a TOML literal.
    value: &'static str,
    /// Settings both runs share so that the key has something to act on, as
    /// `(key, TOML literal)` pairs.
    context: &'static [(&'static str, &'static str)],
    feed: Feed,
}

const fn probe(key: &'static str, value: &'static str) -> Probe {
    Probe {
        key,
        value,
        context: &[],
        feed: Feed::Demo,
    }
}

const fn probe_with(
    key: &'static str,
    value: &'static str,
    context: &'static [(&'static str, &'static str)],
) -> Probe {
    Probe {
        key,
        value,
        context,
        feed: Feed::Demo,
    }
}

const fn silence(key: &'static str, value: &'static str) -> Probe {
    Probe {
        key,
        value,
        context: &[],
        feed: Feed::ToneThenSilence,
    }
}

const fn stereo(key: &'static str, value: &'static str) -> Probe {
    Probe {
        key,
        value,
        context: &[],
        feed: Feed::Stereo,
    }
}

/// Keys with no effect on a frame, and why.
const EXEMPT: &[(&str, &str)] = &[
    (
        "general.live_reload",
        "watches the files while running; covered by the session reload tests",
    ),
    (
        "input.method",
        "chooses the capture backend; headless runs always use the demo or stdin",
    ),
    (
        "input.source",
        "chooses the device behind the backend; covered by the audio source tests",
    ),
    (
        "render.synchronized_output",
        "wraps the terminal writes in a synchronized-update pair; covered by the renderer tests",
    ),
];

#[rustfmt::skip]
fn probes() -> Vec<Probe> {
    vec![
        probe("general.framerate", "20"),
        probe("general.autosens", "false"),
        probe("general.sensitivity", "4.0"),
        silence("general.sleep_timer", "1"),
        stereo("input.channel_mix", "\"left\""),
        stereo("input.sample_rate", "32000"),
        stereo("input.sample_format", "\"f32le\""),
        stereo("input.channels", "1"),
        probe("analysis.fft_size", "1024"),
        probe("analysis.low_cutoff", "400"),
        probe("analysis.high_cutoff", "500"),
        probe("analysis.noise_gate", "0.9"),
        probe("analysis.attack_ms", "1500"),
        probe("analysis.release_ms", "5"),
        probe("plate.shape", "\"circle\""),
        probe("plate.edge", "\"fixed\""),
        probe("plate.symmetry", "\"sym\""),
        probe("plate.max_order", "4"),
        probe_with("plate.rotate_speed", "1.5", &[("plate.shape", "\"circle\"")]),
        probe("mapping.mode", "\"frequency\""),
        probe("mapping.octave_complexity", "false"),
        probe("mapping.base_octave", "5"),
        probe("mapping.min_confidence", "0.99"),
        probe("mapping.hold_ms", "1500"),
        probe("mapping.transition_ms", "1500"),
        probe_with("mapping.peaks", "1", &[("mapping.mode", "\"spectrum\"")]),
        probe("particles.count", "6000"),
        probe_with("particles.density", "0.1", &[("particles.count", "\"auto\"")]),
        probe("particles.step", "4.0"),
        probe("particles.jitter", "0.0"),
        probe("particles.settle", "0.0"),
        probe("particles.inertia", "0.9"),
        probe("particles.kick", "5.0"),
        probe("render.glyphs", "\"ascii\""),
        probe_with("render.ascii_ramp", "\" #\"", &[("render.glyphs", "\"ascii\"")]),
        probe("render.dot_threshold", "3"),
        probe("render.trail", "0.9"),
        probe("render.color_mode", "\"none\""),
        probe_with("render.gradient_steps", "2", &[("colors.theme", "\"nord\"")]),
        probe("render.cell_aspect", "0.35"),
        probe("layout.fit", "\"stretch\""),
        probe("layout.padding", "[0, 0]"),
        probe_with("layout.align", "\"left\"", &[("layout.fit", "\"square\"")]),
        probe("border.style", "\"rounded\""),
        probe_with("border.title", "\"hello\"", &[("border.style", "\"rounded\"")]),
        probe_with(
            "border.title_align",
            "\"left\"",
            &[("border.style", "\"rounded\""), ("border.title", "\"hello\"")],
        ),
        probe("hud.enabled", "true"),
        probe_with("hud.position", "\"top\"", &[("hud.enabled", "true")]),
        probe_with("hud.format", "\"{shape}\"", &[("hud.enabled", "true")]),
        probe("colors.theme", "\"nord\""),
    ]
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        loop {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("grava-keys-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create test directory: {error}"),
            }
        }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Set `dotted` (`section.key`) in the parsed config.
fn set(config: &mut toml::Table, dotted: &str, literal: &str) {
    let (section, key) = dotted.split_once('.').unwrap();
    let value: toml::Value = toml::from_str(&format!("v = {literal}"))
        .map(|table: toml::Table| table["v"].clone())
        .unwrap();
    config
        .entry(section)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .unwrap()
        .insert(key.to_owned(), value);
}

/// The shipped defaults with settings that make every key visible: a small
/// fixed sand count (fast runs), true color so the colors show in the text,
/// and a theme file that can be told apart from the default.
fn shared_base() -> toml::Table {
    let mut config: toml::Table = toml::from_str(DEFAULT_CONFIG).unwrap();
    set(&mut config, "particles.count", "3000");
    set(&mut config, "render.glyphs", "\"braille\"");
    set(&mut config, "render.color_mode", "\"truecolor\"");
    config
}

/// 3 seconds of stereo PCM: 440 Hz left and 330 Hz right at 48 kHz.
fn stereo_pcm() -> Vec<u8> {
    (0..48_000 * 3)
        .flat_map(|index| {
            let time = index as f32 / 48_000.0;
            let tone = |hz: f32| {
                let sample = 0.3 * 32_767.0 * (std::f32::consts::TAU * hz * time).sin();
                (sample as i16).to_le_bytes()
            };
            let (left, right) = (tone(440.0), tone(330.0));
            [left[0], left[1], right[0], right[1]]
        })
        .collect()
}

/// 3 seconds of stereo PCM: half a second of 440 Hz, then digital silence.
fn tone_then_silence_pcm() -> Vec<u8> {
    let tone = stereo_pcm();
    let half_second = 48_000 / 2 * 4;
    let mut pcm = tone[..half_second].to_vec();
    pcm.resize(tone.len(), 0);
    pcm
}

fn render(directory: &TestDirectory, name: &str, config: &toml::Table, feed: Feed) -> String {
    let path = directory.0.join(format!("{name}.toml"));
    fs::write(&path, toml::to_string(config).unwrap()).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_grava"));
    command
        .env("XDG_CONFIG_HOME", directory.0.join("xdg"))
        .env("XDG_RUNTIME_DIR", directory.0.join("runtime"))
        .env("HOME", directory.0.join("home"))
        .env_remove("NO_COLOR")
        .env_remove("PULSE_SERVER")
        .env_remove("PIPEWIRE_REMOTE")
        .arg("--config")
        .arg(&path)
        .args(["--headless", "--size", "100x36", "--seed", "42"]);
    match feed {
        Feed::Demo => command.args(["--input", "demo", "--frames", "150"]),
        Feed::Stereo | Feed::ToneThenSilence => {
            command.args(["--input", "stdin", "--frames", "150"])
        }
    };
    let input = match feed {
        Feed::Demo => Vec::new(),
        Feed::Stereo => stereo_pcm(),
        Feed::ToneThenSilence => tone_then_silence_pcm(),
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(
        output.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn leaf_keys(config: &toml::Table) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for (section, value) in config {
        let table = value
            .as_table()
            .unwrap_or_else(|| panic!("{section} is not a section"));
        for key in table.keys() {
            keys.insert(format!("{section}.{key}"));
        }
    }
    keys
}

#[test]
fn every_default_key_is_probed_or_exempt_with_a_reason() {
    let defaults: toml::Table = toml::from_str(DEFAULT_CONFIG).unwrap();
    let known = leaf_keys(&defaults);
    let probed: BTreeSet<_> = probes().iter().map(|probe| probe.key.to_owned()).collect();
    let exempt: BTreeSet<_> = EXEMPT.iter().map(|(key, _)| (*key).to_owned()).collect();
    assert_eq!(
        probed.len(),
        probes().len(),
        "a key is probed twice: remove the duplicate"
    );
    assert!(
        probed.is_disjoint(&exempt),
        "keys both probed and exempt: {:?}",
        probed.intersection(&exempt).collect::<Vec<_>>()
    );
    let covered: BTreeSet<_> = probed.union(&exempt).cloned().collect();
    let missing: Vec<_> = known.difference(&covered).collect();
    assert!(
        missing.is_empty(),
        "keys with neither a probe nor an exemption: {missing:?}"
    );
    let stale: Vec<_> = covered.difference(&known).collect();
    assert!(
        stale.is_empty(),
        "probes or exemptions for keys the default config no longer has: {stale:?}"
    );
    for (key, reason) in EXEMPT {
        assert!(reason.len() > 20, "{key} needs a real reason");
    }
}

#[test]
fn every_probed_key_changes_what_is_drawn() {
    let directory = TestDirectory::new();
    let all = probes();
    // The runs that probes are compared with, one per distinct context.
    let mut baselines: std::collections::HashMap<(Vec<String>, u8), String> =
        std::collections::HashMap::new();
    let identity = |probe: &Probe| {
        (
            probe
                .context
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>(),
            probe.feed as u8,
        )
    };
    let shared_for = |probe: &Probe| {
        let mut shared = shared_base();
        for (key, literal) in probe.context {
            set(&mut shared, key, literal);
        }
        shared
    };
    for (index, probe) in all.iter().enumerate() {
        baselines.entry(identity(probe)).or_insert_with(|| {
            let baseline = render(
                &directory,
                &format!("baseline-{index}"),
                &shared_for(probe),
                probe.feed,
            );
            // A probe can only be trusted if the run it is compared with draws.
            assert!(
                baseline.chars().any(|c| c.is_alphanumeric()
                    || ('\u{2801}'..='\u{28ff}').contains(&c)
                    || c.is_ascii_punctuation()),
                "{}: the baseline is empty",
                probe.key
            );
            baseline
        });
    }
    // The variants are independent processes: run them side by side.
    let workers = std::thread::available_parallelism().map_or(2, |count| count.get().min(8));
    let failures = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for lane in 0..workers {
            let (all, baselines, directory, failures) = (&all, &baselines, &directory, &failures);
            scope.spawn(move || {
                for probe in all.iter().skip(lane).step_by(workers) {
                    let mut changed = shared_for(probe);
                    set(&mut changed, probe.key, probe.value);
                    let variant = render(
                        directory,
                        &format!("variant-{}", probe.key),
                        &changed,
                        probe.feed,
                    );
                    if variant == baselines[&identity(probe)] {
                        failures.lock().unwrap().push(probe.key);
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "these keys do not change the output: {failures:?}"
    );
}
