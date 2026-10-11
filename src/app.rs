//! Command dispatch and the headless and interactive frame loops.

use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crossterm::event::{self, Event, KeyEventKind};
use signal_hook::{
    consts::{SIGHUP, SIGINT, SIGTERM, SIGUSR1, SIGUSR2},
    flag,
    low_level::unregister,
};

use crate::{
    audio::{AudioError, Pacing},
    cli::Cli,
    config::{
        self, Config, ConfigPaths,
        theme::{self, ThemeLocations},
    },
    engine::Timings,
    pace::Pacer,
    render::{
        color::{ColorLevel, Environment},
        term::{DiffRenderer, TerminalGuard},
    },
    session::{Flow, Reload, Session, SessionSetup},
};

/// Application errors are printed only after the terminal guard has been dropped.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Theme(#[from] theme::ThemeError),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Unsupported(String),
    #[error(transparent)]
    Field(#[from] crate::chladni::FieldError),
    #[error("audio input: {0}")]
    Audio(#[from] AudioError),
}

pub fn run(cli: Cli) -> Result<(), AppError> {
    if cli.print_default_config {
        io::stdout()
            .lock()
            .write_all(config::DEFAULT_CONFIG.as_bytes())?;
        return Ok(());
    }
    if let Some(shell) = cli.completions {
        crate::manual::write_completions(shell, &mut io::stdout().lock());
        return Ok(());
    }
    if cli.man {
        crate::manual::write_man_page(&mut io::stdout().lock())?;
        return Ok(());
    }
    let paths = ConfigPaths::discover(cli.config.as_deref());
    if cli.init {
        return init(&paths, cli.force);
    }
    let loaded = config::load_with_overrides(&paths, |config| cli.apply_overrides(config))?;
    for warning in loaded.warnings {
        writeln!(io::stderr().lock(), "warning: {warning}")?;
    }
    let config = loaded.config;
    let locations = ThemeLocations::new(&paths, &config.colors);
    if cli.check_config {
        // A theme that cannot be found or read is a configuration error too.
        let theme = theme::resolve(&config.colors, &locations)?;
        let mut stdout = io::stdout().lock();
        match loaded.path {
            Some(path) => writeln!(stdout, "Configuration OK: {}", path.display())?,
            None => writeln!(stdout, "Configuration OK: embedded defaults")?,
        }
        writeln!(stdout, "Theme OK: {}", config.colors.theme)?;
        for warning in theme.warnings {
            writeln!(io::stderr().lock(), "warning: {warning}")?;
        }
        return Ok(());
    }
    if cli.list_themes {
        let entries = theme::list(&locations);
        io::stdout()
            .lock()
            .write_all(theme::format_list(&entries, &config.colors.theme).as_bytes())?;
        return Ok(());
    }
    if cli.list_sources {
        let (backend, sources) = crate::audio::select::list_sources(&config.input)?;
        io::stdout()
            .lock()
            .write_all(crate::audio::select::format_sources(backend, &sources).as_bytes())?;
        return Ok(());
    }
    if cli.headless {
        run_headless(&cli, paths, config)
    } else {
        run_interactive(&cli, paths, config)
    }
}

/// `--init`: write the default configuration and a sample theme, refusing to
/// replace anything unless `force` is set. Nothing is written if any target
/// is in the way.
fn init(paths: &ConfigPaths, force: bool) -> Result<(), AppError> {
    let (Some(file), Some(directory)) = (&paths.config_file, &paths.config_dir) else {
        return Err(AppError::Unsupported(
            "no configuration directory: set XDG_CONFIG_HOME or HOME, or pass -c PATH".to_owned(),
        ));
    };
    let (theme_name, theme_text) = theme::SAMPLE_THEME;
    let targets = [
        (file.clone(), config::DEFAULT_CONFIG),
        (
            directory.join("themes").join(format!("{theme_name}.toml")),
            theme_text,
        ),
    ];
    if !force {
        for (path, _) in &targets {
            if path.exists() {
                return Err(AppError::Unsupported(format!(
                    "{} already exists; use --init --force to replace it",
                    path.display()
                )));
            }
        }
    }
    let mut stdout = io::stdout().lock();
    for (path, text) in targets {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, text)?;
        writeln!(stdout, "wrote {}", path.display())?;
    }
    Ok(())
}

fn run_headless(cli: &Cli, paths: ConfigPaths, config: Config) -> Result<(), AppError> {
    let size = cli.size.unwrap_or(crate::cli::TerminalSize {
        width: 80,
        height: 40,
    });
    let dt = 1.0 / config.general.framerate as f32;
    let mut session = Session::start(SessionSetup {
        cli: cli.clone(),
        paths,
        config,
        width: size.width,
        height: size.height,
        pacing: Pacing::Deterministic,
        seed: cli.seed.unwrap_or(0),
        environment: Environment::from_process(),
        watch: false,
    })?;
    for warning in &session.warnings {
        writeln!(io::stderr().lock(), "warning: {warning}")?;
    }
    let mut timings = Timings::default();
    for _ in 0..cli.frames.unwrap_or(1) {
        session.step(dt, cli.bench.then_some(&mut timings))?;
    }
    let export_start = cli.bench.then(Instant::now);
    let frame = session.engine().frame();
    let output = if session.engine().palette().level() == ColorLevel::None {
        frame.plain_text()
    } else {
        frame.ansi_text()
    };
    let mut stdout = io::stdout().lock();
    stdout.write_all(output.as_bytes())?;
    if !output.ends_with('\n') {
        stdout.write_all(b"\n")?;
    }
    if let Some(start) = export_start {
        timings.output += start.elapsed();
    }
    if cli.bench {
        timings.print()?;
    }
    Ok(())
}

/// Signal handlers are installed only for the interactive lifetime.
struct Signals {
    shutdown: Arc<AtomicBool>,
    reload: Arc<AtomicBool>,
    reload_colors: Arc<AtomicBool>,
    handlers: Vec<signal_hook::SigId>,
}

impl Signals {
    fn new() -> io::Result<Self> {
        let mut signals = Self {
            shutdown: Arc::new(AtomicBool::new(false)),
            reload: Arc::new(AtomicBool::new(false)),
            reload_colors: Arc::new(AtomicBool::new(false)),
            handlers: Vec::with_capacity(5),
        };
        for signal in [SIGINT, SIGTERM, SIGHUP] {
            signals
                .handlers
                .push(flag::register(signal, Arc::clone(&signals.shutdown))?);
        }
        // SIGUSR1 reloads everything, SIGUSR2 only the colors, as in cava.
        signals
            .handlers
            .push(flag::register(SIGUSR1, Arc::clone(&signals.reload))?);
        signals
            .handlers
            .push(flag::register(SIGUSR2, Arc::clone(&signals.reload_colors))?);
        Ok(signals)
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for handler in self.handlers.drain(..) {
            unregister(handler);
        }
    }
}

fn run_interactive(cli: &Cli, paths: ConfigPaths, config: Config) -> Result<(), AppError> {
    if !io::stdout().is_terminal() {
        return Err(AppError::Unsupported(
            "interactive mode requires a terminal; use --headless --frames 300 --input demo"
                .to_owned(),
        ));
    }
    // With audio on stdin, keys come from the controlling terminal instead.
    if !io::stdin().is_terminal() && Path::new("/dev/tty").metadata().is_err() {
        return Err(AppError::Unsupported(
            "keyboard input needs a controlling terminal (/dev/tty)".to_owned(),
        ));
    }
    let (width, height) = crossterm::terminal::size()?;
    let frame_time = Duration::from_secs_f64(1.0 / f64::from(config.general.framerate));
    let synchronized = config.render.synchronized_output;
    let mut session = Session::start(SessionSetup {
        cli: cli.clone(),
        paths,
        config,
        width,
        height,
        pacing: Pacing::Realtime,
        seed: cli.seed.unwrap_or(0),
        environment: Environment::from_process(),
        watch: true,
    })?;
    for warning in &session.warnings {
        writeln!(io::stderr().lock(), "warning: {warning}")?;
    }
    let signals = Signals::new()?;
    crate::render::term::install_panic_hook();
    let terminal = TerminalGuard::enter(session.environment().has_alternate_screen())?;
    let mut renderer = DiffRenderer::new();
    let mut timings = Timings::default();
    let mut quit = false;
    let mut pacer = Pacer::new(frame_time, Instant::now());
    while !quit && !signals.shutdown.load(Ordering::Relaxed) {
        let dt = pacer.begin(Instant::now());
        if signals.reload.swap(false, Ordering::Relaxed) {
            session.request_reload(Reload::Everything);
        }
        if signals.reload_colors.swap(false, Ordering::Relaxed) {
            session.request_reload(Reload::Colors);
        }
        // A step with nothing to draw costs almost nothing; the loop then
        // sleeps on the keyboard for a longer tick instead of a frame.
        if session.step(dt, cli.bench.then_some(&mut timings))? {
            let start = cli.bench.then(Instant::now);
            renderer.draw(
                session.engine().frame(),
                &mut io::stdout().lock(),
                synchronized,
            )?;
            if let Some(start) = start {
                timings.output += start.elapsed();
            }
        }
        let resting = session.is_resting();
        let deadline = pacer.schedule(Instant::now(), resting);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !event::poll(remaining)? {
                break;
            }
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if session.handle_key(key.code, key.modifiers)? == Flow::Quit {
                        quit = true;
                    }
                    // Show what the key did now, not at the end of the tick.
                    if resting {
                        pacer.expedite(Instant::now());
                    }
                }
                Event::Resize(width, height) => {
                    session.resize(width, height)?;
                    renderer.invalidate();
                    if resting {
                        pacer.expedite(Instant::now());
                    }
                }
                _ => {}
            }
            if quit
                || signals.shutdown.load(Ordering::Relaxed)
                || Instant::now() >= pacer.deadline()
            {
                break;
            }
        }
    }
    drop(terminal);
    if cli.bench {
        timings.print()?;
    }
    Ok(())
}
