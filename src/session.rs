//! A running grava: the engine, the audio input, and everything that changes
//! them while the program runs (keys, signals, reloads of the configuration
//! and of the theme).
//!
//! The session knows no terminal, so the interactive loop and the tests drive
//! it the same way: report keys and elapsed time, read the frame.

use std::{path::PathBuf, time::Instant};

use crossterm::event::{KeyCode, KeyModifiers};

use crate::{
    app::AppError,
    audio::{self, Pacing, SampleSource},
    chladni::FieldError,
    cli::Cli,
    config::{
        self, Config, ConfigPaths,
        theme::{self, ResolvedTheme, ThemeLocations},
        watch::Watcher,
    },
    engine::{Engine, Setup, Timings},
    render::{color::Environment, hud::NoticeKind},
};

/// What a key can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Reload,
    ReloadColors,
    SensitivityUp,
    SensitivityDown,
    SimplerFigures,
    MoreIntricateFigures,
    NextTheme,
    PreviousTheme,
    TogglePlate,
    CycleMapping,
    CycleGlyphs,
    Scatter,
    Freeze,
    ToggleHud,
    Help,
}

/// The action of a key press, if it has one.
pub fn action_for(code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
    let control = modifiers.contains(KeyModifiers::CONTROL);
    Some(match code {
        KeyCode::Esc | KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('c') if control => Action::Quit,
        KeyCode::Char('c') => Action::ReloadColors,
        KeyCode::Char('r') => Action::Reload,
        KeyCode::Up => Action::SensitivityUp,
        KeyCode::Down => Action::SensitivityDown,
        KeyCode::Left => Action::SimplerFigures,
        KeyCode::Right => Action::MoreIntricateFigures,
        KeyCode::Char('t') => Action::NextTheme,
        KeyCode::Char('T') => Action::PreviousTheme,
        KeyCode::Char('p') => Action::TogglePlate,
        KeyCode::Char('m') => Action::CycleMapping,
        KeyCode::Char('g') => Action::CycleGlyphs,
        KeyCode::Char('s') => Action::Scatter,
        KeyCode::Char(' ') => Action::Freeze,
        KeyCode::Char('i') => Action::ToggleHud,
        KeyCode::Char('?') => Action::Help,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

/// How much a reload replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reload {
    /// The colors only: `[colors]`, `render.color_mode`, `render.gradient_steps`.
    Colors,
    Everything,
}

/// How much sensitivity one press of an arrow key adds or takes away.
const SENSITIVITY_STEP: f32 = 0.1;

pub struct SessionSetup {
    pub cli: Cli,
    pub paths: ConfigPaths,
    /// The configuration, with the command-line options already applied.
    pub config: Config,
    pub width: u16,
    pub height: u16,
    pub pacing: Pacing,
    pub seed: u64,
    pub environment: Environment,
    /// Reload when the files change (if `general.live_reload` allows it).
    pub watch: bool,
}

pub struct Session {
    cli: Cli,
    paths: ConfigPaths,
    pacing: Pacing,
    seed: u64,
    environment: Environment,
    engine: Engine,
    source: Box<dyn SampleSource>,
    samples: Vec<f32>,
    sample_rate: u32,
    locations: ThemeLocations,
    watcher: Watcher,
    watching: bool,
    pending: Option<Reload>,
    /// Problems with the theme at startup that are worth a line on stderr.
    pub warnings: Vec<String>,
}

impl Session {
    /// Open the audio input, resolve the theme, and build the engine. Errors
    /// here are reported to the user before the terminal is touched.
    pub fn start(setup: SessionSetup) -> Result<Self, AppError> {
        let SessionSetup {
            cli,
            paths,
            config,
            width,
            height,
            pacing,
            seed,
            environment,
            watch,
        } = setup;
        let locations = ThemeLocations::new(&paths, &config.colors);
        let resolved = theme::resolve(&config.colors, &locations)?;
        let opened = audio::open_source(&config.input, seed, pacing)?;
        let sample_rate = opened.source.sample_rate();
        let theme_file = resolved.watched_file().map(PathBuf::from);
        let theme_name = config.colors.theme.clone();
        let mut engine = Engine::new(Setup {
            config,
            scheme: resolved.scheme,
            environment: environment.clone(),
            width,
            height,
            seed,
            sample_rate,
        })?;
        engine.set_labels(&theme_name, &opened.label);
        Ok(Self {
            watcher: Watcher::new(paths.config_file.clone(), theme_file),
            watching: watch,
            samples: Vec::with_capacity(sample_rate as usize),
            warnings: resolved.warnings,
            cli,
            paths,
            pacing,
            seed,
            environment,
            engine,
            source: opened.source,
            sample_rate,
            locations,
            pending: None,
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn resize(&mut self, width: u16, height: u16) -> Result<(), FieldError> {
        self.engine.resize(width, height)
    }

    pub fn environment(&self) -> &Environment {
        &self.environment
    }

    /// Ask for a reload at the next step; used by the signal handlers.
    pub fn request_reload(&mut self, scope: Reload) {
        self.pending = Some(match (self.pending, scope) {
            (Some(Reload::Everything), _) | (_, Reload::Everything) => Reload::Everything,
            _ => Reload::Colors,
        });
    }

    /// Pull one frame's worth of audio, apply what is pending (a requested or
    /// detected reload), and advance the engine. Returns whether the frame
    /// changed and has to be drawn.
    pub fn step(&mut self, dt: f32, timings: Option<&mut Timings>) -> Result<bool, AppError> {
        let mut timings = timings;
        let started = timings.is_some().then(Instant::now);
        self.samples.clear();
        self.source.pull(dt, &mut self.samples)?;
        self.engine.feed(&self.samples);
        self.engine.set_connected(self.source.connected());
        if let (Some(started), Some(timings)) = (started, timings.as_deref_mut()) {
            timings.input += started.elapsed();
        }
        if self.watching && self.engine.config().general.live_reload {
            let changes = self.watcher.poll(dt);
            if changes.config {
                self.request_reload(Reload::Everything);
            } else if changes.theme {
                self.request_reload(Reload::Colors);
            }
        }
        if let Some(scope) = self.pending.take() {
            self.reload(scope);
        }
        Ok(self.engine.step(dt, timings)?)
    }

    /// Whether the last step had nothing to draw, so that the next one can wait.
    pub fn is_resting(&self) -> bool {
        self.engine.is_resting()
    }

    /// A key press: close the help if it is open, otherwise do what the key says.
    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<Flow, AppError> {
        // Any key brings a sleeping plate back, and the sleep timer starts over.
        self.engine.wake();
        let action = action_for(code, modifiers);
        if self.engine.help_open() && action != Some(Action::Quit) {
            self.engine.close_help();
            // Esc and q only close the help; every other key is spent doing so,
            // as the help itself says.
            return Ok(Flow::Continue);
        }
        match action {
            Some(action) => self.apply(action),
            None => Ok(Flow::Continue),
        }
    }

    pub fn apply(&mut self, action: Action) -> Result<Flow, AppError> {
        match action {
            Action::Quit => return Ok(Flow::Quit),
            Action::Freeze => self.engine.toggle_pause(),
            Action::Scatter => self.engine.scatter(),
            Action::SensitivityUp => {
                self.engine.adjust_sensitivity(SENSITIVITY_STEP);
            }
            Action::SensitivityDown => {
                self.engine.adjust_sensitivity(-SENSITIVITY_STEP);
            }
            Action::SimplerFigures => self.engine.shift_tier(-1),
            Action::MoreIntricateFigures => self.engine.shift_tier(1),
            Action::NextTheme => self.change_theme(true),
            Action::PreviousTheme => self.change_theme(false),
            Action::TogglePlate => self.engine.toggle_shape()?,
            Action::CycleMapping => self.engine.cycle_mapping()?,
            Action::CycleGlyphs => self.engine.cycle_glyphs()?,
            Action::ToggleHud => self.engine.toggle_hud()?,
            Action::Help => self.engine.toggle_help(),
            Action::Reload => self.request_reload(Reload::Everything),
            Action::ReloadColors => self.request_reload(Reload::Colors),
        }
        Ok(Flow::Continue)
    }

    /// Switch to the next or previous theme of the cycle.
    fn change_theme(&mut self, forward: bool) {
        let mut config = self.engine.config().clone();
        config.colors.theme = theme::cycle(&config.colors.theme, &self.locations, forward);
        match theme::resolve(&config.colors, &self.locations) {
            Ok(resolved) => {
                let name = config.colors.theme.clone();
                self.adopt_theme(resolved, &config);
                self.engine
                    .notify(NoticeKind::Info, format!("theme: {name}"));
            }
            Err(error) => self.engine.notify(NoticeKind::Error, error.to_string()),
        }
    }

    fn adopt_theme(&mut self, resolved: ResolvedTheme, config: &Config) {
        self.watcher
            .watch_theme(resolved.watched_file().map(PathBuf::from));
        self.engine.set_colors(resolved.scheme, config);
    }

    /// Reload from disk. A broken file leaves everything as it was and
    /// reports the problem on the status row.
    fn reload(&mut self, scope: Reload) {
        match self.try_reload(scope) {
            Ok(warning) => {
                let what = match scope {
                    Reload::Everything => "configuration reloaded",
                    Reload::Colors => "colors reloaded",
                };
                let text = match warning {
                    Some(warning) => format!("{what}; warning: {warning}"),
                    None => what.to_owned(),
                };
                self.engine.notify(NoticeKind::Info, text);
            }
            Err(message) => self
                .engine
                .notify(NoticeKind::Error, format!("reload failed: {message}")),
        }
        // Whatever is on disk now has been looked at.
        self.watcher.rebase();
    }

    /// Returns the first warning, if any.
    fn try_reload(&mut self, scope: Reload) -> Result<Option<String>, String> {
        let loaded = config::load_with_overrides(&self.paths, |config| {
            self.cli.apply_overrides(config);
        })
        .map_err(|error| error.to_string())?;
        let locations = ThemeLocations::new(&self.paths, &loaded.config.colors);
        let resolved =
            theme::resolve(&loaded.config.colors, &locations).map_err(|error| error.to_string())?;
        let mut warnings = loaded.warnings;
        warnings.extend(resolved.warnings.iter().cloned());
        match scope {
            Reload::Colors => {
                self.locations = locations;
                self.adopt_theme(resolved, &loaded.config);
            }
            Reload::Everything => {
                let mut label = None;
                if loaded.config.input != self.engine.config().input {
                    let opened = audio::open_source(&loaded.config.input, self.seed, self.pacing)
                        .map_err(|error| error.to_string())?;
                    if opened.source.sample_rate() != self.sample_rate {
                        self.sample_rate = opened.source.sample_rate();
                        self.engine.set_sample_rate(self.sample_rate);
                    }
                    self.source = opened.source;
                    label = Some(opened.label);
                }
                self.engine
                    .reconfigure(loaded.config.clone())
                    .map_err(|error| error.to_string())?;
                self.locations = locations;
                self.adopt_theme(resolved, &loaded.config);
                if let Some(label) = label {
                    self.engine.set_labels(&loaded.config.colors.theme, &label);
                }
            }
        }
        let theme = self.engine.config().colors.theme.clone();
        self.engine.set_theme_label(&theme);
        Ok(warnings.into_iter().next())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::render::color::ColorLevel;
    use std::fs;

    struct Workspace(PathBuf);

    impl Workspace {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("grava-session-{}-{label}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join("grava/themes")).unwrap();
            fs::write(path.join("grava/config.toml"), "").unwrap();
            Self(path)
        }

        fn config_file(&self) -> PathBuf {
            self.0.join("grava/config.toml")
        }

        fn write_config(&self, text: &str) {
            fs::write(self.config_file(), text).unwrap();
        }

        fn write_theme(&self, name: &str, text: &str) {
            fs::write(
                self.0.join("grava/themes").join(format!("{name}.toml")),
                text,
            )
            .unwrap();
        }

        /// A session on the demo input in this workspace; the config file is
        /// read as the command-line `-c` option would name it.
        fn session(&self, watch: bool) -> Session {
            self.session_with(&["-i", "demo"], watch)
        }

        fn session_with(&self, args: &[&str], watch: bool) -> Session {
            let paths = ConfigPaths::discover(Some(&self.config_file()));
            let cli = <Cli as clap::Parser>::try_parse_from(
                ["grava", "--glyphs", "braille", "--seed", "5"]
                    .into_iter()
                    .chain(args.iter().copied()),
            )
            .unwrap();
            let loaded =
                config::load_with_overrides(&paths, |config| cli.apply_overrides(config)).unwrap();
            Session::start(SessionSetup {
                cli,
                paths,
                config: loaded.config,
                width: 60,
                height: 30,
                pacing: Pacing::Deterministic,
                seed: 5,
                environment: Environment {
                    locale: Some("en_US.UTF-8".to_owned()),
                    term: Some("xterm-256color".to_owned()),
                    colorterm: Some("truecolor".to_owned()),
                    no_color: false,
                },
                watch,
            })
            .unwrap()
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const DT: f32 = 1.0 / 60.0;

    fn run(session: &mut Session, frames: usize) {
        for _ in 0..frames {
            session.step(DT, None).unwrap();
        }
    }

    fn notice(session: &Session) -> String {
        session
            .engine()
            .notice()
            .map(|notice| notice.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn a_key_wakes_a_sleeping_plate_and_a_resting_one_reports_it() {
        let workspace = Workspace::new("sleep");
        workspace.write_config("[general]\nsleep_timer = 1\n");
        // Two seconds of digital silence as 48 kHz stereo s16le.
        let silence = workspace.0.join("silence.pcm");
        fs::write(&silence, vec![0_u8; 48_000 * 4 * 2]).unwrap();
        let mut session =
            workspace.session_with(&["-i", "fifo", "-s", silence.to_str().unwrap()], false);
        run(&mut session, 30);
        assert!(!session.engine().is_asleep());
        run(&mut session, 60);
        assert!(session.engine().is_asleep());
        assert!(
            !session.step(DT, None).unwrap(),
            "a sleeping plate draws nothing"
        );
        assert!(session.is_resting());
        // Any key brings it back, even one that is bound to nothing.
        session
            .handle_key(KeyCode::Char('x'), KeyModifiers::NONE)
            .unwrap();
        assert!(!session.engine().is_asleep());
        assert!(session.step(DT, None).unwrap());
        assert!(!session.is_resting());
        assert!(
            session
                .engine()
                .frame()
                .cells()
                .iter()
                .any(|cell| cell.character != ' ')
        );
    }

    #[test]
    fn keys_map_to_the_documented_actions() {
        let none = KeyModifiers::NONE;
        let table = [
            (KeyCode::Char('q'), Action::Quit),
            (KeyCode::Esc, Action::Quit),
            (KeyCode::Char('r'), Action::Reload),
            (KeyCode::Char('c'), Action::ReloadColors),
            (KeyCode::Up, Action::SensitivityUp),
            (KeyCode::Down, Action::SensitivityDown),
            (KeyCode::Left, Action::SimplerFigures),
            (KeyCode::Right, Action::MoreIntricateFigures),
            (KeyCode::Char('t'), Action::NextTheme),
            (KeyCode::Char('T'), Action::PreviousTheme),
            (KeyCode::Char('p'), Action::TogglePlate),
            (KeyCode::Char('m'), Action::CycleMapping),
            (KeyCode::Char('g'), Action::CycleGlyphs),
            (KeyCode::Char('s'), Action::Scatter),
            (KeyCode::Char(' '), Action::Freeze),
            (KeyCode::Char('i'), Action::ToggleHud),
            (KeyCode::Char('?'), Action::Help),
        ];
        for (code, action) in table {
            assert_eq!(action_for(code, none), Some(action), "{code:?}");
        }
        // Ctrl-C quits, a bare c reloads colors; shift does not hide a key.
        assert_eq!(
            action_for(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
        assert_eq!(
            action_for(KeyCode::Char('T'), KeyModifiers::SHIFT),
            Some(Action::PreviousTheme)
        );
        assert_eq!(action_for(KeyCode::Char('x'), none), None);
        assert_eq!(action_for(KeyCode::Enter, none), None);
    }

    #[test]
    fn the_demo_session_paints_sand_and_the_keys_change_what_they_say() {
        let workspace = Workspace::new("keys");
        let mut session = workspace.session(false);
        run(&mut session, 120);
        let before = session.engine().frame().clone();
        assert!(before.plain_text().chars().any(|c| c != ' ' && c != '\n'));
        // Freeze stops the picture; thaw lets it move again.
        session.apply(Action::Freeze).unwrap();
        let frozen = session.engine().frame().clone();
        run(&mut session, 30);
        // (A freeze notice is not shown: freezing says nothing, it just stops.)
        assert_eq!(session.engine().frame().plain_text(), frozen.plain_text());
        session.apply(Action::Freeze).unwrap();
        run(&mut session, 30);
        assert_ne!(session.engine().frame().plain_text(), frozen.plain_text());
        // Sensitivity moves in tenths and says so.
        session.apply(Action::SensitivityUp).unwrap();
        assert_eq!(session.engine().config().general.sensitivity, 1.1);
        assert_eq!(notice(&session), "sensitivity 1.1");
        for _ in 0..3 {
            session.apply(Action::SensitivityDown).unwrap();
        }
        assert_eq!(session.engine().config().general.sensitivity, 0.8);
        // Quit is the only key that ends the session.
        assert_eq!(session.apply(Action::Quit).unwrap(), Flow::Quit);
        assert_eq!(session.apply(Action::Scatter).unwrap(), Flow::Continue);
    }

    #[test]
    fn plate_mapping_glyph_and_hud_keys_reconfigure_the_running_engine() {
        let workspace = Workspace::new("toggles");
        let mut session = workspace.session(false);
        run(&mut session, 90);
        session.apply(Action::TogglePlate).unwrap();
        assert_eq!(
            format!("{:?}", session.engine().config().plate.shape),
            "Circle"
        );
        assert_eq!(notice(&session), "circle plate");
        run(&mut session, 90);
        session.apply(Action::TogglePlate).unwrap();
        assert_eq!(
            format!("{:?}", session.engine().config().plate.shape),
            "Square"
        );
        session.apply(Action::CycleMapping).unwrap();
        assert_eq!(notice(&session), "mapping: frequency");
        session.apply(Action::CycleMapping).unwrap();
        session.apply(Action::CycleMapping).unwrap();
        assert_eq!(notice(&session), "mapping: note");
        let mut seen = vec![session.engine().glyphs().name()];
        for _ in 0..4 {
            session.apply(Action::CycleGlyphs).unwrap();
            seen.push(session.engine().glyphs().name());
            run(&mut session, 5);
        }
        assert_eq!(seen, ["braille", "quadrant", "half", "ascii", "braille"]);
        assert!(!session.engine().config().hud.enabled);
        session.apply(Action::ToggleHud).unwrap();
        assert!(session.engine().config().hud.enabled);
        run(&mut session, 300);
        assert!(
            session
                .engine()
                .frame()
                .plain_text()
                .lines()
                .last()
                .unwrap()
                .trim()
                .len()
                > 3
        );
        // The figure keys shift the tier and say so.
        session.apply(Action::MoreIntricateFigures).unwrap();
        assert_eq!(notice(&session), "figure tier +1");
        session.apply(Action::SimplerFigures).unwrap();
        session.apply(Action::SimplerFigures).unwrap();
        assert_eq!(notice(&session), "figure tier -1");
    }

    #[test]
    fn the_help_overlay_opens_closes_on_any_key_and_never_traps_quit() {
        let workspace = Workspace::new("help");
        let mut session = workspace.session(false);
        run(&mut session, 5);
        session.apply(Action::Help).unwrap();
        assert!(session.engine().help_open());
        run(&mut session, 2);
        assert!(session.engine().frame().plain_text().contains("grava keys"));
        // A key such as `s` closes the help and does nothing else.
        assert_eq!(
            session
                .handle_key(KeyCode::Char('s'), KeyModifiers::NONE)
                .unwrap(),
            Flow::Continue
        );
        assert!(!session.engine().help_open());
        run(&mut session, 2);
        assert!(!session.engine().frame().plain_text().contains("grava keys"));
        // Ctrl-C still quits from within the help.
        session.apply(Action::Help).unwrap();
        assert_eq!(
            session
                .handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL)
                .unwrap(),
            Flow::Quit
        );
        // `?` closes it again.
        assert_eq!(
            session
                .handle_key(KeyCode::Char('?'), KeyModifiers::NONE)
                .unwrap(),
            Flow::Continue
        );
        assert!(!session.engine().help_open());
    }

    #[test]
    fn theme_keys_cycle_through_the_themes_and_report_them() {
        let workspace = Workspace::new("themes");
        workspace.write_theme("zeta", "sand = \"#010203\"\ncolorize = \"solid\"\n");
        let mut session = workspace.session(false);
        run(&mut session, 5);
        session.apply(Action::NextTheme).unwrap();
        assert_eq!(notice(&session), "theme: sand");
        assert_eq!(session.engine().config().colors.theme, "sand");
        session.apply(Action::PreviousTheme).unwrap();
        session.apply(Action::PreviousTheme).unwrap();
        assert_eq!(session.engine().config().colors.theme, "zeta");
        assert_eq!(
            session.engine().palette().colorize(),
            crate::config::schema::Colorize::Solid
        );
        assert!(session.engine().palette().level() >= ColorLevel::Ansi256);
        session.apply(Action::NextTheme).unwrap();
        assert_eq!(session.engine().config().colors.theme, "default");
        // A broken theme file is reported and skipped over, not fatal.
        workspace.write_theme("zeta", "sand = \"nonsense\"\n");
        session.apply(Action::PreviousTheme).unwrap();
        assert!(notice(&session).contains("zeta"), "{}", notice(&session));
        run(&mut session, 3);
    }

    #[test]
    fn a_reload_picks_up_the_file_and_a_broken_file_changes_nothing() {
        let workspace = Workspace::new("reload");
        workspace.write_config("[hud]\nenabled = true\n[particles]\ncount = 3000\n");
        let mut session = workspace.session(false);
        run(&mut session, 5);
        assert_eq!(session.engine().particle_count(), 3000);
        workspace.write_config(
            "[hud]\nenabled = true\n[particles]\ncount = 5000\n[colors]\ntheme = \"nord\"\n",
        );
        session.apply(Action::Reload).unwrap();
        run(&mut session, 2);
        assert_eq!(session.engine().particle_count(), 5000);
        assert_eq!(session.engine().config().colors.theme, "nord");
        assert_eq!(notice(&session), "configuration reloaded");
        // A syntax error keeps the previous configuration and says why.
        workspace.write_config("[particles\ncount = 1\n");
        session.apply(Action::Reload).unwrap();
        run(&mut session, 2);
        assert!(
            notice(&session).starts_with("reload failed:"),
            "{}",
            notice(&session)
        );
        assert_eq!(session.engine().particle_count(), 5000);
        // So does a value that is invalid, and the error names the key.
        workspace.write_config("[general]\nframerate = 0\n");
        session.apply(Action::Reload).unwrap();
        run(&mut session, 2);
        assert!(
            notice(&session).contains("general.framerate"),
            "{}",
            notice(&session)
        );
        // And an unknown theme.
        workspace.write_config("[colors]\ntheme = \"nowhere\"\n");
        session.apply(Action::Reload).unwrap();
        run(&mut session, 2);
        assert!(
            notice(&session).contains("unknown theme"),
            "{}",
            notice(&session)
        );
        assert_eq!(session.engine().config().colors.theme, "nord");
        // Unknown keys are only a warning.
        workspace.write_config("[particles]\ncount = 4000\nbogus = 1\n");
        session.apply(Action::Reload).unwrap();
        run(&mut session, 2);
        assert!(
            notice(&session).contains("warning") && notice(&session).contains("particles.bogus"),
            "{}",
            notice(&session)
        );
        assert_eq!(session.engine().particle_count(), 4000);
    }

    #[test]
    fn command_line_options_beat_the_file_even_after_a_reload() {
        let workspace = Workspace::new("cli");
        workspace.write_config("[render]\nglyphs = \"ascii\"\n[general]\nsensitivity = 2.0\n");
        let mut session = workspace.session(false);
        // The session was started with `--glyphs braille`.
        assert_eq!(session.engine().glyphs().name(), "braille");
        assert_eq!(session.engine().config().general.sensitivity, 2.0);
        session.apply(Action::SensitivityUp).unwrap();
        workspace.write_config("[render]\nglyphs = \"ascii\"\n[general]\nsensitivity = 3.0\n");
        session.apply(Action::Reload).unwrap();
        run(&mut session, 2);
        assert_eq!(session.engine().glyphs().name(), "braille");
        // The file's own settings do win over what the keys had changed.
        assert_eq!(session.engine().config().general.sensitivity, 3.0);
    }

    #[test]
    fn colors_only_reloads_leave_the_rest_alone() {
        let workspace = Workspace::new("colors");
        workspace.write_config("[particles]\ncount = 3000\n");
        let mut session = workspace.session(false);
        run(&mut session, 5);
        session.apply(Action::Scatter).unwrap();
        workspace.write_config(
            "[particles]\ncount = 9000\n[colors]\ntheme = \"gruvbox-dark\"\nsand = \"#112233\"\n",
        );
        session.apply(Action::ReloadColors).unwrap();
        run(&mut session, 2);
        assert_eq!(notice(&session), "colors reloaded");
        assert_eq!(session.engine().config().colors.theme, "gruvbox-dark");
        assert_eq!(
            session.engine().particle_count(),
            3000,
            "only colors reload"
        );
        assert_eq!(
            session.engine().palette().sand(),
            crate::render::color::to_term(
                "#112233".parse().unwrap(),
                session.engine().palette().level()
            )
        );
    }

    #[test]
    fn live_reload_follows_the_files_when_enabled() {
        let workspace = Workspace::new("live");
        workspace.write_theme("mine", "sand = \"#aaaaaa\"\ncolorize = \"solid\"\n");
        workspace.write_config("[colors]\ntheme = \"mine\"\n");
        let mut session = workspace.session(true);
        run(&mut session, 40);
        assert!(notice(&session).is_empty());
        // Editing the theme file reloads the colors.
        workspace.write_theme("mine", "sand = \"#bbbbbb\"\ncolorize = \"solid\"\n");
        run(&mut session, 40);
        assert_eq!(notice(&session), "colors reloaded");
        let sand = |session: &Session| session.engine().palette().sand();
        assert_eq!(
            sand(&session),
            crate::render::color::to_term(
                "#bbbbbb".parse().unwrap(),
                session.engine().palette().level()
            )
        );
        // Editing the configuration reloads everything.
        workspace.write_config("[colors]\ntheme = \"mine\"\n[particles]\ncount = 2500\n");
        run(&mut session, 40);
        assert_eq!(notice(&session), "configuration reloaded");
        assert_eq!(session.engine().particle_count(), 2500);
        // Quiet files stay quiet.
        run(&mut session, 400);
        assert!(notice(&session).is_empty());
        // `live_reload = false` is respected, while the keys still work.
        workspace.write_config("[general]\nlive_reload = false\n");
        run(&mut session, 40);
        assert_eq!(notice(&session), "configuration reloaded");
        let automatic = session.engine().particle_count();
        assert_ne!(automatic, 2500);
        workspace.write_config("[general]\nlive_reload = false\n[particles]\ncount = 2200\n");
        run(&mut session, 60);
        assert_eq!(
            session.engine().particle_count(),
            automatic,
            "not polled any more"
        );
        session.apply(Action::Reload).unwrap();
        run(&mut session, 2);
        assert_eq!(session.engine().particle_count(), 2200);
        // Without watching at all, nothing is polled.
        let mut headless = workspace.session(false);
        let before = headless.engine().particle_count();
        workspace.write_config("[general]\nlive_reload = true\n[particles]\ncount = 2100\n");
        run(&mut headless, 80);
        assert_eq!(headless.engine().particle_count(), before);
    }

    #[test]
    fn requested_reloads_merge_and_the_wider_one_wins() {
        let workspace = Workspace::new("merge");
        let mut session = workspace.session(false);
        session.request_reload(Reload::Colors);
        session.request_reload(Reload::Everything);
        session.request_reload(Reload::Colors);
        assert_eq!(session.pending, Some(Reload::Everything));
        run(&mut session, 2);
        assert_eq!(session.pending, None);
        assert_eq!(notice(&session), "configuration reloaded");
    }
}
