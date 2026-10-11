//! End-to-end checks of configuration and deterministic headless visualization.

#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use grava::config::{Config, DEFAULT_CONFIG};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        loop {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("grava-m0-{}-{id}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create test directory: {error}"),
            }
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_grava"));
        // Tests never reach the developer's real audio server: with an empty
        // runtime directory there is no PulseAudio or PipeWire socket to find.
        command
            .env("XDG_CONFIG_HOME", self.0.join("xdg"))
            .env("XDG_RUNTIME_DIR", self.0.join("runtime"))
            .env("HOME", self.0.join("home"))
            .env("NO_COLOR", "1")
            .env_remove("PULSE_SERVER")
            .env_remove("PULSE_RUNTIME_PATH")
            .env_remove("PIPEWIRE_REMOTE")
            .env_remove("PIPEWIRE_RUNTIME_DIR");
        command
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn printed_defaults_are_exact_valid_toml_and_ignore_local_config() {
    let directory = TestDirectory::new();
    let output = directory
        .command()
        .args(["--config", "/does/not/exist", "--print-default-config"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(output.stdout, DEFAULT_CONFIG.as_bytes());
    let config: Config = toml::from_str(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
    assert_eq!(config, Config::default());
    config.validate().unwrap();
}

#[test]
fn absent_implicit_config_uses_defaults_and_creates_nothing() {
    let directory = TestDirectory::new();
    let output = directory.command().arg("--check-config").output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("embedded defaults"));
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
}

#[test]
fn explicit_missing_file_is_an_error() {
    let directory = TestDirectory::new();
    let path = directory.0.join("missing.toml");
    let output = directory
        .command()
        .arg("--config")
        .arg(&path)
        .arg("--check-config")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("missing.toml"));
}

#[test]
fn xdg_config_is_loaded_and_cli_overrides_are_validated() {
    let directory = TestDirectory::new();
    let config_dir = directory.0.join("xdg/grava");
    fs::create_dir_all(&config_dir).unwrap();
    // A semantic error in a setting superseded by the CLI must not reject the
    // effective configuration. Syntax/type errors cannot be overridden.
    fs::write(
        config_dir.join("config.toml"),
        "[general]\nframerate = 0\n[particles]\ncount = 0\n",
    )
    .unwrap();
    let output = directory
        .command()
        .args([
            "--check-config",
            "--fps",
            "90",
            "--particles",
            "5000",
            "--input",
            "demo",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("xdg/grava/config.toml"));
    let invalid = directory
        .command()
        .args(["--check-config", "--fps", "0"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(stderr(&invalid).contains("fps") || stderr(&invalid).contains("general.framerate"));
}

#[test]
fn home_fallback_and_symlinked_config_are_supported() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new();
    let config_dir = directory.0.join("home/.config/grava");
    fs::create_dir_all(&config_dir).unwrap();
    let target = directory.0.join("managed.toml");
    fs::write(&target, "[general]\nframerate = 75\n").unwrap();
    symlink(&target, config_dir.join("config.toml")).unwrap();
    let output = directory
        .command()
        .env_remove("XDG_CONFIG_HOME")
        .arg("--check-config")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("home/.config/grava/config.toml"));
}

#[test]
fn unknown_keys_warn_and_invalid_values_fail_with_context() {
    let directory = TestDirectory::new();
    let path = directory.0.join("config.toml");
    fs::write(&path, "[general]\nframrate = 90\n").unwrap();
    let output = directory
        .command()
        .arg("--config")
        .arg(&path)
        .arg("--check-config")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("warning: unknown configuration key `general.framrate`"));

    fs::write(&path, "[analysis]\nfft_size = 3000\n").unwrap();
    let invalid = directory
        .command()
        .arg("--config")
        .arg(&path)
        .arg("--check-config")
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(stderr(&invalid).contains("analysis.fft_size"));
    assert!(stderr(&invalid).contains("config.toml"));
}

#[test]
fn headless_frames_are_deterministic_and_have_exact_dimensions() {
    let directory = TestDirectory::new();
    let render = |seed: &str, bench: bool| {
        let mut command = directory.command();
        command.args([
            "--headless",
            "--frames",
            "150",
            "--input",
            "demo",
            "--glyphs",
            "braille",
            "--size",
            "40x20",
            "--seed",
            seed,
            "--no-color",
        ]);
        if bench {
            command.arg("--bench");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        output
    };
    let first = render("42", false);
    let repeated = render("42", false);
    let different_seed = render("43", false);
    let benchmarked = render("42", true);
    assert_eq!(first.stdout, repeated.stdout);
    assert_eq!(first.stdout, benchmarked.stdout);
    assert_ne!(first.stdout, different_seed.stdout);
    let text = std::str::from_utf8(&first.stdout).unwrap();
    assert_eq!(text.lines().count(), 20);
    assert!(text.lines().all(|line| line.chars().count() == 40));
    assert!(
        text.chars()
            .any(|character| ('\u{2801}'..='\u{28ff}').contains(&character))
    );
    assert!(!first.stdout.contains(&0x1b));
    assert!(first.stderr.is_empty());
    assert!(stderr(&benchmarked).contains("simulation"));
}

#[test]
fn tiny_headless_terminal_and_unimplemented_inputs_are_handled_cleanly() {
    let directory = TestDirectory::new();
    let tiny = directory
        .command()
        .args([
            "--headless",
            "--frames",
            "2",
            "--input",
            "demo",
            "--size",
            "6x3",
        ])
        .output()
        .unwrap();
    assert!(tiny.status.success(), "{}", stderr(&tiny));
    assert_eq!(
        String::from_utf8(tiny.stdout).unwrap(),
        "      \nTermin\n      \n"
    );
    // Live audio cannot be replayed, so headless runs refuse it and say what
    // can be used instead.
    for method in ["auto", "pulse", "pipewire", "alsa"] {
        let unsupported = directory
            .command()
            .args(["--headless", "--frames", "1", "--input", method])
            .output()
            .unwrap();
        assert!(!unsupported.status.success(), "{method}");
        let message = stderr(&unsupported);
        assert!(
            message.contains("reproducible") && message.contains("demo"),
            "{message}"
        );
        assert!(unsupported.stdout.is_empty());
        assert!(!unsupported.stderr.contains(&0x1b));
    }
}

/// Stereo s16le at 48 kHz: a sine of `hz` for `seconds`, then nothing.
fn tone(hz: f32, seconds: f32) -> Vec<u8> {
    (0..(48_000.0 * seconds) as usize)
        .flat_map(|index| {
            let sample =
                (0.3 * 32_767.0 * (std::f32::consts::TAU * hz * index as f32 / 48_000.0).sin())
                    as i16;
            let bytes = sample.to_le_bytes();
            [bytes[0], bytes[1], bytes[0], bytes[1]]
        })
        .collect()
}

/// Run grava headless with `input` on stdin, as `parec ... | grava` would.
fn headless_stdin(directory: &TestDirectory, input: Vec<u8>, extra: &[&str]) -> Output {
    let mut child = directory
        .command()
        .args([
            "--headless",
            "--input",
            "stdin",
            "--size",
            "60x30",
            "--seed",
            "7",
        ])
        .args(["--glyphs", "braille", "--no-color"])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // Feed from a thread so a full pipe can never deadlock the test.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    output
}

fn lit_cells(output: &Output) -> usize {
    String::from_utf8_lossy(&output.stdout)
        .chars()
        .filter(|character| ('\u{2801}'..='\u{28ff}').contains(character))
        .count()
}

#[test]
fn stdin_pcm_is_analyzed_deterministically_and_draws_sand() {
    let directory = TestDirectory::new();
    let frames = ["--frames", "180"];
    let first = headless_stdin(&directory, tone(440.0, 3.0), &frames);
    let again = headless_stdin(&directory, tone(440.0, 3.0), &frames);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(first.stderr.is_empty(), "{}", stderr(&first));
    assert_eq!(first.stdout, again.stdout);
    assert_eq!(String::from_utf8_lossy(&first.stdout).lines().count(), 30);
    assert!(lit_cells(&first) > 100);
    // A different note gives a different figure from the same sand.
    let other = headless_stdin(&directory, tone(330.0, 3.0), &frames);
    assert_ne!(first.stdout, other.stdout);
    // Silence leaves the sand scattered and still; so does an empty stream.
    let silent = headless_stdin(&directory, vec![0; 48_000 * 4 * 3], &frames);
    let empty = headless_stdin(&directory, Vec::new(), &frames);
    assert!(silent.status.success() && empty.status.success());
    // An input that ended says so on the status row; the sand is the same.
    let sand = |output: &Output| {
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.lines().take(29).collect::<Vec<_>>().join("\n")
    };
    assert_eq!(sand(&silent), sand(&empty));
    assert!(String::from_utf8_lossy(&empty.stdout).contains("audio input lost"));
    assert!(!String::from_utf8_lossy(&silent.stdout).contains("audio input lost"));
    assert_ne!(silent.stdout, first.stdout);
}

#[test]
fn fifo_input_reads_the_same_pcm_as_stdin() {
    let directory = TestDirectory::new();
    let pcm = tone(440.0, 3.0);
    let from_stdin = headless_stdin(&directory, pcm.clone(), &["--frames", "120"]);
    let path = directory.0.join("tone.pcm");
    fs::write(&path, &pcm).unwrap();
    let from_file = directory
        .command()
        .args(["--headless", "--input", "fifo", "--source"])
        .arg(&path)
        .args(["--frames", "120", "--size", "60x30", "--seed", "7"])
        .args(["--glyphs", "braille", "--no-color"])
        .output()
        .unwrap();
    assert!(from_file.status.success(), "{}", stderr(&from_file));
    assert_eq!(from_file.stdout, from_stdin.stdout);
    // The raw format is configurable: mono f32 is a different stream.
    let floats: Vec<u8> = (0..48_000 * 3)
        .flat_map(|index| {
            (0.3 * (std::f32::consts::TAU * 440.0 * index as f32 / 48_000.0).sin()).to_le_bytes()
        })
        .collect();
    fs::write(&path, floats).unwrap();
    let mono = directory
        .command()
        .args(["--headless", "--input", "fifo", "--source"])
        .arg(&path)
        .args(["--frames", "120", "--size", "60x30", "--seed", "7"])
        .args(["--glyphs", "braille", "--no-color"])
        .env("XDG_CONFIG_HOME", directory.0.join("cfg"))
        .output()
        .unwrap();
    // Read as stereo s16le by default, the floats are noise rather than a tone.
    assert!(mono.status.success());
    assert_ne!(mono.stdout, from_file.stdout);
}

#[test]
fn a_missing_fifo_is_reported_with_advice_before_anything_starts() {
    let directory = TestDirectory::new();
    let output = directory
        .command()
        .args(["--headless", "--frames", "1", "--input", "fifo", "--source"])
        .arg(directory.0.join("absent.fifo"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("mkfifo"), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
}

#[test]
fn the_drum_head_stays_round_and_is_deterministic() {
    let directory = TestDirectory::new();
    let render = |seed: &str| {
        let output = directory
            .command()
            .args(["--headless", "--frames", "240", "--input", "demo"])
            .args([
                "--shape", "circle", "--glyphs", "braille", "--size", "40x20",
            ])
            .args(["--seed", seed, "--no-color"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        output
    };
    let first = render("42");
    assert_eq!(first.stdout, render("42").stdout);
    assert_ne!(first.stdout, render("43").stdout);
    let text = String::from_utf8(first.stdout).unwrap();
    let rows: Vec<Vec<char>> = text.lines().map(|line| line.chars().collect()).collect();
    assert_eq!(rows.len(), 20);
    assert!(rows.iter().flatten().filter(|c| **c != ' ').count() > 50);
    // The plate occupies x 2..38, y 1..19; the corners lie outside the drum.
    for (x, y) in [(2, 1), (37, 1), (2, 18), (37, 18)] {
        assert_eq!(rows[y][x], ' ', "corner ({x}, {y})");
    }
}

#[test]
fn bench_reports_the_audio_stage() {
    let directory = TestDirectory::new();
    let output = directory
        .command()
        .args(["--headless", "--frames", "30", "--input", "demo", "--bench"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let report = stderr(&output);
    for stage in ["audio", "field", "simulation", "rasterize", "glyphs"] {
        assert!(report.contains(stage), "{report}");
    }
}

#[test]
fn listing_sources_without_any_audio_server_explains_every_attempt() {
    let directory = TestDirectory::new();
    let output = directory.command().arg("--list-sources").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let message = stderr(&output);
    for expected in [
        "no audio backend could be opened",
        "pipewire:",
        "pulse:",
        "alsa:",
        "pipewire-pulse",
        "--input stdin",
    ] {
        assert!(
            message.contains(expected),
            "{expected:?} missing from:\n{message}"
        );
    }
    // The attempts appear in the documented order.
    assert!(message.find("pipewire:").unwrap() < message.find("pulse:").unwrap());
    assert!(message.find("pulse:").unwrap() < message.find("alsa:").unwrap());
    // Backends left out of the build say which feature brings them back.
    if !cfg!(feature = "pipewire") {
        assert!(message.contains("--features pipewire"), "{message}");
    }
    if !cfg!(feature = "alsa") {
        assert!(message.contains("--features alsa"), "{message}");
    }
}

#[test]
fn an_explicit_backend_reports_only_its_own_failure() {
    let directory = TestDirectory::new();
    let output = directory
        .command()
        .args(["--list-sources", "--input", "pulse"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("pulse:"), "{message}");
    assert!(
        !message.contains("pipewire:") && !message.contains("alsa:"),
        "{message}"
    );
    if !cfg!(feature = "pulse") {
        assert!(message.contains("--features pulse"), "{message}");
        return;
    }
    assert!(message.contains("pipewire-pulse"), "{message}");
    // A remote server named in PULSE_SERVER is refused with a clear reason.
    let remote = directory
        .command()
        .env("PULSE_SERVER", "tcp:audio.example:4713")
        .args(["--list-sources", "--input", "pulse"])
        .output()
        .unwrap();
    assert!(
        stderr(&remote).contains("remote server"),
        "{}",
        stderr(&remote)
    );
}

#[test]
fn version_describes_feature_flags_honestly() {
    let directory = TestDirectory::new();
    let output = directory.command().arg("--version").output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let version = String::from_utf8_lossy(&output.stdout);
    assert!(version.contains("grava 0.1.0"));
    assert!(version.contains("features"));
    if cfg!(feature = "pulse") {
        assert!(version.contains("pulse"));
    }
    if cfg!(feature = "pipewire") {
        assert!(version.contains("pipewire"));
    }
    if cfg!(feature = "alsa") {
        assert!(version.contains("alsa"));
    }
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn listing_themes_marks_the_active_one_and_finds_user_files() {
    let directory = TestDirectory::new();
    let themes = directory.0.join("xdg/grava/themes");
    fs::create_dir_all(&themes).unwrap();
    fs::write(themes.join("mine.toml"), "sand = \"#112233\"\n").unwrap();
    let output = directory.command().arg("--list-themes").output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let listing = stdout_text(&output);
    for name in [
        "default",
        "sand",
        "mono",
        "catppuccin-mocha",
        "gruvbox-dark",
        "nord",
        "tokyo-night",
        "rose-pine",
        "mine",
        "pywal",
    ] {
        assert!(listing.contains(name), "{name} missing from:\n{listing}");
    }
    assert!(listing.contains("mine.toml"), "{listing}");
    let starred = |listing: &str| -> Vec<String> {
        listing
            .lines()
            .filter(|line| line.starts_with('*'))
            .map(|line| line.split_whitespace().nth(1).unwrap().to_owned())
            .collect()
    };
    assert_eq!(starred(&listing), ["default"]);
    let output = directory
        .command()
        .args(["-t", "mine", "--list-themes"])
        .output()
        .unwrap();
    assert_eq!(starred(&stdout_text(&output)), ["mine"]);
    // Listing writes nothing.
    assert!(!directory.0.join("xdg/grava/config.toml").exists());
}

#[test]
fn an_unknown_theme_is_an_error_that_lists_the_choices() {
    let directory = TestDirectory::new();
    let output = directory
        .command()
        .args(["--theme", "nosuch", "--check-config"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("nosuch"), "{message}");
    assert!(message.contains("nord"), "{message}");
    // A theme file with a broken color names the file and the key.
    let file = directory.0.join("broken.toml");
    fs::write(&file, "sand = \"not-a-color\"\n").unwrap();
    let output = directory
        .command()
        .arg("--theme")
        .arg(&file)
        .arg("--check-config")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(
        message.contains("broken.toml") && message.contains("sand"),
        "{message}"
    );
}

#[test]
fn init_writes_a_config_and_a_sample_theme_and_never_replaces_silently() {
    let directory = TestDirectory::new();
    let config = directory.0.join("xdg/grava/config.toml");
    let theme = directory.0.join("xdg/grava/themes/dune.toml");
    let output = directory.command().arg("--init").output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fs::read_to_string(&config).unwrap(), DEFAULT_CONFIG);
    assert!(fs::read_to_string(&theme).unwrap().contains("gradient"));
    let listing = stdout_text(&output);
    assert!(
        listing.contains("config.toml") && listing.contains("dune.toml"),
        "{listing}"
    );
    // The files it wrote are valid and the sample theme can be used by name.
    let output = directory
        .command()
        .args(["-t", "dune", "--check-config"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout_text(&output).contains("Theme OK: dune"));
    // A second run refuses and leaves the user's edits alone.
    fs::write(&config, "[general]\nframerate = 30\n").unwrap();
    let output = directory.command().arg("--init").output().unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("--force"), "{}", stderr(&output));
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        "[general]\nframerate = 30\n"
    );
    // A leftover sample theme alone also stops it, before anything is written.
    fs::remove_file(&config).unwrap();
    let output = directory.command().arg("--init").output().unwrap();
    assert!(!output.status.success());
    assert!(!config.exists());
    // --force replaces both.
    fs::write(&theme, "sand = \"#000000\"\n").unwrap();
    let output = directory
        .command()
        .args(["--init", "--force"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fs::read_to_string(&config).unwrap(), DEFAULT_CONFIG);
    assert!(fs::read_to_string(&theme).unwrap().contains("gradient"));
}

/// Run a deterministic colored demo.
fn headless_colors(
    directory: &TestDirectory,
    environment: &[(&str, &str)],
    extra: &[&str],
) -> Output {
    let mut command = directory.command();
    command
        .env_remove("NO_COLOR")
        .env_remove("COLORTERM")
        .env_remove("TERM");
    for (name, value) in environment {
        command.env(name, value);
    }
    let output = command
        .args([
            "--headless",
            "--frames",
            "150",
            "--input",
            "demo",
            "--size",
            "60x30",
            "--seed",
            "5",
        ])
        .args(extra)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    output
}

#[test]
fn headless_colors_follow_the_theme_and_the_terminal() {
    let directory = TestDirectory::new();
    let truecolor = [("COLORTERM", "truecolor"), ("TERM", "xterm-256color")];
    let rich = headless_colors(&directory, &truecolor, &["--theme", "nord"]);
    let text = stdout_text(&rich);
    assert!(text.contains("\x1b[38;2;"), "truecolor sequences expected");
    // Cells keep the frame's shape: stripping the escapes leaves 30 rows of 60.
    let plain = headless_colors(&directory, &[], &["--no-color", "--theme", "nord"]);
    assert!(!plain.stdout.contains(&0x1b));
    let strip = |text: &str| -> String {
        let mut out = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    };
    assert_eq!(strip(&text), stdout_text(&plain));
    // Another theme gives other colors on the same sand.
    let gruvbox = stdout_text(&headless_colors(
        &directory,
        &truecolor,
        &["--theme", "gruvbox-dark"],
    ));
    assert_ne!(text, gruvbox);
    assert_eq!(strip(&gruvbox), stdout_text(&plain));
    // A 256-color terminal gets indexed colors, a 16-color one the basic set.
    let indexed = stdout_text(&headless_colors(
        &directory,
        &[("TERM", "xterm-256color")],
        &["--theme", "nord"],
    ));
    assert!(indexed.contains("\x1b[38;5;") && !indexed.contains("\x1b[38;2;"));
    let basic = stdout_text(&headless_colors(
        &directory,
        &[("TERM", "linux")],
        &["--theme", "nord"],
    ));
    assert!(!basic.contains("\x1b[38;5;") && !basic.contains("\x1b[38;2;"));
    assert!(
        basic.contains("\x1b[9") || basic.contains("\x1b[3"),
        "{basic:?}"
    );
    // NO_COLOR wins in auto mode.
    let no_color = headless_colors(
        &directory,
        &[("NO_COLOR", "1"), ("COLORTERM", "truecolor")],
        &[],
    );
    assert!(!no_color.stdout.contains(&0x1b));
    // The default theme uses the terminal's own palette, never RGB.
    let native = stdout_text(&headless_colors(&directory, &truecolor, &[]));
    assert!(
        native.contains("\x1b[") && !native.contains("38;2;"),
        "{native:?}"
    );
}

#[test]
fn every_glyph_set_and_border_can_be_drawn_headless() {
    let directory = TestDirectory::new();
    for (glyphs, allowed) in [("quadrant", " ▘▝▀▖▌▞▛▗▚▐▜▄▙▟█"), ("half", " ▀▄█")]
    {
        let output = headless_colors(&directory, &[], &["--no-color", "--glyphs", glyphs]);
        let text = stdout_text(&output);
        assert_eq!(text.lines().count(), 30, "{glyphs}");
        assert!(
            text.lines().all(|line| line.chars().count() == 60),
            "{glyphs}"
        );
        assert!(
            text.chars()
                .filter(|c| *c != '\n')
                .all(|c| allowed.contains(c)),
            "{glyphs} drew characters outside its set"
        );
        assert!(text.chars().any(|c| c != ' ' && c != '\n'), "{glyphs}");
    }
    let ascii = stdout_text(&headless_colors(
        &directory,
        &[],
        &["--no-color", "--glyphs", "ascii"],
    ));
    assert!(ascii.is_ascii());
    assert!(ascii.chars().any(|c| c.is_ascii_graphic()));
    // A box border and a title come from the configuration file.
    let file = directory.0.join("border.toml");
    fs::write(
        &file,
        "[border]\nstyle = \"double\"\ntitle = \" {shape} \"\n[hud]\nenabled = true\nformat = \"{theme}\"\n",
    )
    .unwrap();
    let output = headless_colors(
        &directory,
        &[],
        &[
            "--no-color",
            "--glyphs",
            "braille",
            "--config",
            file.to_str().unwrap(),
        ],
    );
    let text = stdout_text(&output);
    assert!(
        text.contains('╔') && text.contains('╝') && text.contains("square"),
        "{text}"
    );
    assert!(text.lines().last().unwrap().contains("default"), "{text}");
    assert_eq!(text.lines().count(), 30);
}

#[test]
fn completions_and_the_man_page_are_generated_without_touching_the_configuration() {
    let directory = TestDirectory::new();
    for (shell, marker) in [
        ("bash", "_grava"),
        ("zsh", "#compdef grava"),
        ("fish", "complete -c grava"),
    ] {
        let output = directory
            .command()
            .args(["--config", "/does/not/exist", "--completions", shell])
            .output()
            .unwrap();
        assert!(output.status.success(), "{shell}: {}", stderr(&output));
        let script = stdout_text(&output);
        assert!(script.contains(marker), "{shell}: {script:.200}");
        assert!(
            script.contains("list-themes") && script.contains("headless"),
            "{shell}"
        );
    }
    let output = directory
        .command()
        .args(["--config", "/does/not/exist", "--man"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let page = stdout_text(&output);
    assert!(page.contains(".TH grava 1"));
    assert!(page.contains(".SH KEYS") && page.contains(".SH SIGNALS"));
    assert!(
        !page.contains("enabled audio features"),
        "the title takes one line"
    );
    // Neither command creates anything.
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
    let bad = directory
        .command()
        .args(["--completions", "powershell"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(stderr(&bad).contains("bash"), "{}", stderr(&bad));
}

#[test]
fn the_linux_console_gets_ascii_and_basic_colors_by_itself() {
    let directory = TestDirectory::new();
    let output = headless_colors(
        &directory,
        &[("TERM", "linux"), ("LANG", "en_US.UTF-8")],
        &["--theme", "nord"],
    );
    let text = stdout_text(&output);
    // Escapes aside, nothing but ASCII reaches the console font.
    let mut plain = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    assert!(plain.is_ascii(), "the console has no Braille");
    assert!(plain.chars().any(|c| c.is_ascii_graphic()));
    assert!(text.contains("\x1b[") && !text.contains("38;5;") && !text.contains("38;2;"));
    // The same terminal with a modern TERM draws Braille.
    let modern = headless_colors(
        &directory,
        &[("TERM", "xterm-256color"), ("LANG", "en_US.UTF-8")],
        &["--theme", "nord"],
    );
    assert!(
        stdout_text(&modern)
            .chars()
            .any(|c| ('\u{2801}'..='\u{28ff}').contains(&c))
    );
    // A locale that is not UTF-8 gets ASCII too.
    let c_locale = headless_colors(
        &directory,
        &[("TERM", "xterm-256color"), ("LANG", "C"), ("LC_ALL", "C")],
        &["--no-color"],
    );
    assert!(stdout_text(&c_locale).is_ascii());
}
