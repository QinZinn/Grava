//! The per-frame pipeline: audio features drive the mapping, the mapping
//! drives the vibration field, the field drives the sand, and the sand is
//! rasterized into a frame of terminal cells with its colors, border, status
//! line and overlays.
//!
//! The engine owns no terminal and reads no clock: callers pass the time step,
//! which makes headless runs reproducible. Everything that can change while
//! the program runs (a reloaded configuration, a key press) goes through
//! [`Engine::reconfigure`] or one of the action methods.

use std::{
    f32::consts::TAU,
    io::{self, Write},
    time::{Duration, Instant},
};

use crate::{
    chladni::{ActiveField, FieldCache, FieldError, Mapper},
    config::{
        Config,
        schema::{BorderStyle, Colorize, HudPosition, MappingMode, ParticleCount, Shape},
        template::{self, Segment},
        theme::ColorScheme,
    },
    dsp::{Analyzer, AudioFeatures},
    render::{
        border::{self, Rect},
        canvas::Canvas,
        color::{ColorLevel, Environment},
        colorize::{CellInput, Palette},
        frame::Frame,
        glyphs::GlyphSet,
        hud::{
            self, Emphasis, EnergyMeter, LineColors, Notice, NoticeKind, StatusValues, TextLine,
        },
        layout::{Margins, PlateLayout},
    },
    sim::Particles,
};

/// Per-stage wall-clock totals for `--bench`.
#[derive(Debug, Default)]
pub struct Timings {
    pub frames: u64,
    /// Steps that found nothing to redraw and did almost no work.
    pub idle: u64,
    /// Pulling and feeding the input, on every step.
    pub input: Duration,
    /// What the engine spent on the idle steps.
    pub resting: Duration,
    pub audio: Duration,
    pub field: Duration,
    pub simulation: Duration,
    pub rasterize: Duration,
    pub glyphs: Duration,
    pub output: Duration,
}

impl Timings {
    pub fn print(&self) -> io::Result<()> {
        let micros =
            |total: Duration, count: u64| total.as_secs_f64() * 1.0e6 / count.max(1) as f64;
        let per_frame = |total: Duration| total.as_secs_f64() * 1000.0 / self.frames.max(1) as f64;
        writeln!(
            io::stderr().lock(),
            "bench: {} frames; audio {:.3} ms; field {:.3} ms; simulation {:.3} ms; \
             rasterize {:.3} ms; glyphs {:.3} ms; output {:.3} ms per frame",
            self.frames,
            per_frame(self.audio),
            per_frame(self.field),
            per_frame(self.simulation),
            per_frame(self.rasterize),
            per_frame(self.glyphs),
            per_frame(self.output)
        )?;
        writeln!(
            io::stderr().lock(),
            "bench: {} idle steps; input {:.1} us per step, engine {:.1} us per idle step",
            self.idle,
            micros(self.input, self.frames + self.idle),
            micros(self.resting, self.idle),
        )
    }
}

/// Measures one stage and adds it to a running total, when benchmarking.
struct Stopwatch(Option<Instant>);

impl Stopwatch {
    fn start(enabled: bool) -> Self {
        Self(enabled.then(Instant::now))
    }

    fn lap(&mut self, total: Option<&mut Duration>) {
        if let (Some(started), Some(total)) = (self.0, total) {
            *total += started.elapsed();
            self.0 = Some(Instant::now());
        }
    }
}

/// Everything an engine is built from.
pub struct Setup {
    pub config: Config,
    pub scheme: ColorScheme,
    pub environment: Environment,
    /// The terminal size in cells.
    pub width: u16,
    pub height: u16,
    pub seed: u64,
    /// The rate of the samples later passed to [`Engine::feed`].
    pub sample_rate: u32,
}

/// Where the plate and its border sit in the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    layout: Option<PlateLayout>,
    /// The line-drawing box around the plate, for box border styles.
    outline: Option<Rect>,
}

/// Follows the pitch class that has held for `hold_ms`, which is the note
/// whose color the plate takes: a note that flickers by for a moment does not
/// flash the sand.
#[derive(Debug, Default)]
struct NoteTracker {
    current: Option<u8>,
    candidate: Option<u8>,
    held_ms: f32,
}

impl NoteTracker {
    /// Returns the new note when it changes.
    fn update(&mut self, heard: Option<u8>, hold_ms: f32, dt: f32) -> Option<u8> {
        let Some(class) = heard else {
            self.candidate = None;
            return None;
        };
        if self.current == Some(class) {
            self.candidate = None;
            return None;
        }
        if self.candidate == Some(class) {
            self.held_ms += dt * 1000.0;
        } else {
            self.candidate = Some(class);
            self.held_ms = 0.0;
        }
        if self.held_ms >= hold_ms {
            self.current = Some(class);
            self.candidate = None;
            return Some(class);
        }
        None
    }
}

impl NoteTracker {
    /// Whether no note is waiting to be held, so that nothing can change
    /// without new sound.
    fn is_idle(&self) -> bool {
        self.candidate.is_none()
    }
}

/// The fading between the colors of two notes.
#[derive(Debug)]
struct NoteColor {
    from: Option<u8>,
    to: Option<u8>,
    progress: f32,
}

impl NoteColor {
    fn change(&mut self, class: u8) {
        // A change in the middle of a fade starts from the side nearer to
        // where the fade has got.
        self.from = if self.progress < 0.5 {
            self.from
        } else {
            self.to
        };
        self.to = Some(class);
        self.progress = 0.0;
    }

    fn is_settled(&self) -> bool {
        self.progress >= 1.0
    }

    fn advance(&mut self, dt: f32, transition_ms: f32) {
        self.progress = if transition_ms > 0.0 {
            (self.progress + dt * 1000.0 / transition_ms).min(1.0)
        } else {
            1.0
        };
    }
}

pub struct Engine {
    config: Config,
    environment: Environment,
    glyphs: GlyphSet,
    scheme: ColorScheme,
    palette: Palette,
    sample_rate: u32,
    size: (u16, u16),
    analyzer: Analyzer,
    mapper: Mapper,
    geometry: Geometry,
    particles: Particles,
    canvas: Option<Canvas>,
    cache: FieldCache,
    field: ActiveField,
    frame: Frame,
    features: AudioFeatures,
    rotation: f32,
    paused: bool,
    dirty: bool,
    /// Seconds without sound; drives `general.sleep_timer`.
    silent_for: f32,
    /// Frames drawn since the sound stopped, so that fading afterimages
    /// have time to finish before the engine rests.
    quiet_frames: u32,
    /// Whether the last frame drawn was the blank one of the sleeping plate.
    shown_asleep: bool,
    /// Whether the last step found nothing to redraw.
    resting: bool,
    /// Resting can be switched off to compare against the full pipeline.
    rest_allowed: bool,

    theme: String,
    source: String,
    connected: bool,
    fps: f32,
    meter: EnergyMeter,
    meter_text: String,
    title_line: TextLine,
    last_note: Option<(u8, i8, f32)>,
    note_tracker: NoteTracker,
    note_color: NoteColor,
    hud_template: Vec<Segment>,
    title_template: Vec<Segment>,
    line: TextLine,
    notice: Option<Notice>,
    help: bool,
}

impl Engine {
    pub fn new(setup: Setup) -> Result<Self, FieldError> {
        let Setup {
            config,
            scheme,
            environment,
            width,
            height,
            seed,
            sample_rate,
        } = setup;
        let glyphs = resolve_glyphs(&config, &environment);
        let palette = make_palette(&config, &scheme, &environment);
        let mut engine = Self {
            analyzer: Analyzer::new(&config.analysis, &config.general, sample_rate),
            mapper: Mapper::new(&config, 1, 1),
            geometry: Geometry {
                layout: None,
                outline: None,
            },
            particles: Particles::new_with_shape(0, seed, &config.particles, config.plate.shape),
            canvas: None,
            cache: FieldCache::for_plate(1, 1),
            field: ActiveField::for_plate(1, 1),
            frame: Frame::new(width, height),
            features: AudioFeatures::default(),
            rotation: 0.0,
            paused: false,
            dirty: true,
            silent_for: 0.0,
            quiet_frames: 0,
            shown_asleep: false,
            resting: false,
            rest_allowed: true,
            theme: config.colors.theme.clone(),
            source: String::new(),
            connected: true,
            fps: 0.0,
            meter: EnergyMeter::default(),
            meter_text: String::new(),
            title_line: TextLine::new(),
            last_note: None,
            note_tracker: NoteTracker::default(),
            note_color: NoteColor {
                from: None,
                to: None,
                progress: 1.0,
            },
            hud_template: parse_template(&config.hud.format),
            title_template: parse_template(&config.border.title),
            line: TextLine::new(),
            notice: None,
            help: false,
            size: (width, height),
            sample_rate,
            palette,
            scheme,
            glyphs,
            environment,
            config,
        };
        engine.relayout()?;
        Ok(engine)
    }

    // --- Accessors ---------------------------------------------------------

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn features(&self) -> &AudioFeatures {
        &self.features
    }

    pub fn mapper(&self) -> &Mapper {
        &self.mapper
    }

    pub fn glyphs(&self) -> GlyphSet {
        self.glyphs
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn particle_count(&self) -> usize {
        self.particles.len()
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn help_open(&self) -> bool {
        self.help
    }

    /// Whether the last step found nothing to redraw. A loop can then wait
    /// longer before the next one: the picture only changes with sound, a
    /// key, a notice or a reload, and the first of those is noticed within
    /// one wait.
    pub fn is_resting(&self) -> bool {
        self.resting
    }

    /// Whether `general.sleep_timer` has run out and the plate is blank.
    pub fn is_asleep(&self) -> bool {
        let timer = self.config.general.sleep_timer;
        timer > 0 && self.silent_for >= timer as f32
    }

    /// Show the plate again and restart the sleep timer, as a key press does.
    pub fn wake(&mut self) {
        self.silent_for = 0.0;
        self.dirty = true;
    }

    /// Stop skipping the work of a step that cannot change the picture.
    #[cfg(test)]
    fn disable_rest(&mut self) {
        self.rest_allowed = false;
    }

    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// Whether the plate fits at all at the current terminal size.
    pub fn has_plate(&self) -> bool {
        self.geometry.layout.is_some()
    }

    /// The plate's rectangle in cells, `(x, y, width, height)`.
    pub fn plate_rect(&self) -> Option<(u16, u16, u16, u16)> {
        self.geometry
            .layout
            .map(|layout| (layout.x, layout.y, layout.width, layout.height))
    }

    // --- Status the app reports ------------------------------------------

    /// Tell the HUD which theme is active and where the audio comes from.
    pub fn set_labels(&mut self, theme: &str, source: &str) {
        if self.theme != theme || self.source != source {
            self.theme = theme.to_owned();
            self.source = source.to_owned();
            self.dirty = true;
        }
    }

    /// Name the active theme in the HUD.
    pub fn set_theme_label(&mut self, theme: &str) {
        if self.theme != theme {
            self.theme = theme.to_owned();
            self.dirty = true;
        }
    }

    /// The input now delivers samples at `rate`; the analysis starts afresh.
    pub fn set_sample_rate(&mut self, rate: u32) {
        self.sample_rate = rate;
        self.analyzer = Analyzer::new(&self.config.analysis, &self.config.general, rate);
    }

    /// Tell the HUD whether audio is flowing; a change shows a notice.
    pub fn set_connected(&mut self, connected: bool) {
        if connected != self.connected {
            self.connected = connected;
            if connected {
                self.notify(NoticeKind::Info, "audio input restored");
            } else {
                self.notify(NoticeKind::Error, "audio input lost");
            }
        }
    }

    /// Show `text` on the status row for a few seconds.
    pub fn notify(&mut self, kind: NoticeKind, text: impl Into<String>) {
        self.notice = Some(Notice::new(kind, text));
        self.wake();
    }

    // --- Input -------------------------------------------------------------

    /// Hand mono samples to the analyzer. A frozen engine drops them, so that
    /// unfreezing does not replay stale audio.
    pub fn feed(&mut self, samples: &[f32]) {
        if !self.paused {
            self.analyzer.push(samples);
        }
    }

    // --- Actions of the keys ----------------------------------------------

    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.dirty = true;
    }

    /// Re-scatter all the sand uniformly.
    pub fn scatter(&mut self) {
        self.particles.scatter();
        self.dirty = true;
    }

    pub fn toggle_help(&mut self) {
        self.help = !self.help;
        if self.help && self.size.0 < hud::help_size(self.glyphs.is_unicode()).0 {
            self.help = false;
            self.notify(NoticeKind::Error, "terminal too narrow for the help");
        }
        self.dirty = true;
    }

    pub fn close_help(&mut self) {
        if self.help {
            self.help = false;
            self.dirty = true;
        }
    }

    /// Change the sensitivity by `delta`, within 0..10. Returns the new value.
    pub fn adjust_sensitivity(&mut self, delta: f32) -> f32 {
        let value = ((self.config.general.sensitivity + delta) * 10.0).round() / 10.0;
        let value = value.clamp(0.0, 10.0);
        self.config.general.sensitivity = value;
        self.analyzer.set_sensitivity(value);
        self.notify(NoticeKind::Info, format!("sensitivity {value:.1}"));
        value
    }

    /// Make the figures one tier more intricate (`delta` > 0) or simpler.
    pub fn shift_tier(&mut self, delta: i32) {
        let shift = self.mapper.shift_tier(delta);
        self.notify(NoticeKind::Info, format!("figure tier {shift:+}"));
    }

    pub fn toggle_shape(&mut self) -> Result<(), FieldError> {
        let mut config = self.config.clone();
        config.plate.shape = match config.plate.shape {
            Shape::Square => Shape::Circle,
            Shape::Circle => Shape::Square,
        };
        let name = hud_shape(config.plate.shape);
        self.reconfigure(config)?;
        self.notify(NoticeKind::Info, format!("{name} plate"));
        Ok(())
    }

    pub fn cycle_mapping(&mut self) -> Result<(), FieldError> {
        let mut config = self.config.clone();
        let (mode, name) = match config.mapping.mode {
            MappingMode::Note => (MappingMode::Frequency, "frequency"),
            MappingMode::Frequency => (MappingMode::Spectrum, "spectrum"),
            MappingMode::Spectrum => (MappingMode::Note, "note"),
        };
        config.mapping.mode = mode;
        self.reconfigure(config)?;
        self.notify(NoticeKind::Info, format!("mapping: {name}"));
        Ok(())
    }

    pub fn cycle_glyphs(&mut self) -> Result<(), FieldError> {
        let next = self.glyphs.next();
        let mut config = self.config.clone();
        config.render.glyphs = next.config_value();
        self.reconfigure(config)?;
        self.notify(NoticeKind::Info, format!("glyphs: {}", next.name()));
        Ok(())
    }

    pub fn toggle_hud(&mut self) -> Result<(), FieldError> {
        let mut config = self.config.clone();
        config.hud.enabled = !config.hud.enabled;
        self.reconfigure(config)
    }

    /// Switch to another color scheme, and to the color settings of `config`
    /// (the `[colors]` table, `render.color_mode` and `render.gradient_steps`),
    /// leaving everything else as it is.
    pub fn set_colors(&mut self, scheme: ColorScheme, colors: &Config) {
        self.config.colors = colors.colors.clone();
        self.config.render.color_mode = colors.render.color_mode;
        self.config.render.gradient_steps = colors.render.gradient_steps;
        self.scheme = scheme;
        self.palette = make_palette(&self.config, &self.scheme, &self.environment);
        self.dirty = true;
    }

    /// Apply a new configuration to the running engine, rebuilding only what
    /// the differences require. The sand and the current figure carry over
    /// wherever the plate stays the same.
    pub fn reconfigure(&mut self, config: Config) -> Result<(), FieldError> {
        let old = std::mem::replace(&mut self.config, config);
        let new = self.config.clone();
        if old.analysis != new.analysis
            || old.general.autosens != new.general.autosens
            || old.general.framerate != new.general.framerate
        {
            self.analyzer = Analyzer::new(&new.analysis, &new.general, self.sample_rate);
        } else if old.general.sensitivity != new.general.sensitivity {
            self.analyzer.set_sensitivity(new.general.sensitivity);
        }
        if old.plate != new.plate
            || old.mapping != new.mapping
            || old.analysis.low_cutoff != new.analysis.low_cutoff
            || old.analysis.high_cutoff != new.analysis.high_cutoff
        {
            let previous = std::mem::replace(&mut self.mapper, Mapper::new(&new, 1, 1));
            self.mapper.adopt(previous);
        }
        self.glyphs = resolve_glyphs(&new, &self.environment);
        if old.render.color_mode != new.render.color_mode
            || old.render.gradient_steps != new.render.gradient_steps
            || old.colors != new.colors
        {
            self.palette = make_palette(&new, &self.scheme, &self.environment);
        }
        self.hud_template = parse_template(&new.hud.format);
        self.title_template = parse_template(&new.border.title);
        if new.colors.theme != old.colors.theme {
            self.theme.clone_from(&new.colors.theme);
        }
        let layout_changed = old.render.glyphs != new.render.glyphs
            || old.render.ascii_ramp != new.render.ascii_ramp
            || old.render.dot_threshold != new.render.dot_threshold
            || old.render.trail != new.render.trail
            || old.render.cell_aspect != new.render.cell_aspect
            || old.layout != new.layout
            || old.border.style != new.border.style
            || old.hud.enabled != new.hud.enabled
            || old.hud.position != new.hud.position
            || old.plate != new.plate
            || old.mapping != new.mapping
            || old.particles != new.particles;
        if layout_changed {
            self.relayout()?;
        }
        self.dirty = true;
        Ok(())
    }

    pub fn resize(&mut self, width: u16, height: u16) -> Result<(), FieldError> {
        self.size = (width, height);
        self.frame = Frame::new(width, height);
        self.relayout()
    }

    // --- Layout ------------------------------------------------------------

    /// The border drawn for the current plate: a circle border needs a drum.
    fn border_style(&self) -> BorderStyle {
        match (self.config.border.style, self.config.plate.shape) {
            (BorderStyle::Circle, Shape::Square) => BorderStyle::Rounded,
            (style, _) => style,
        }
    }

    fn plan(&self) -> Geometry {
        let style = self.border_style();
        let ring = u16::from(border::box_chars(style, self.glyphs.is_unicode()).is_some());
        let hud = u16::from(self.config.hud.enabled);
        let (hud_top, hud_bottom) = match self.config.hud.position {
            HudPosition::Top => (hud, 0),
            HudPosition::Bottom => (0, hud),
        };
        let layout = PlateLayout::new(
            self.size.0,
            self.size.1,
            &self.config.layout,
            self.config.render.cell_aspect,
            self.glyphs,
            Margins {
                top: hud_top + ring,
                bottom: hud_bottom + ring,
                left: ring,
                right: ring,
            },
        );
        Geometry {
            layout,
            outline: layout
                .filter(|_| ring == 1)
                .map(|layout| Rect::around(&layout)),
        }
    }

    /// The plate's size in sub-pixels, or 1x1 when there is no plate.
    fn plate_size(&self) -> (usize, usize) {
        self.geometry.layout.map_or((1, 1), |layout| {
            (layout.subpixel_width, layout.subpixel_height)
        })
    }

    /// Rebuild everything that depends on the terminal size, the glyph set or
    /// the plate. The sand keeps its places and the mapper its figure.
    fn relayout(&mut self) -> Result<(), FieldError> {
        self.geometry = self.plan();
        self.frame = Frame::new(self.size.0, self.size.1);
        self.canvas = self.geometry.layout.map(|layout| {
            let mut canvas = Canvas::new(layout, &self.config.render, self.glyphs);
            if self.config.plate.shape == Shape::Circle {
                // A drum covers pi/4 of its square: denser cells at the same count.
                canvas.set_plate_fraction(std::f32::consts::FRAC_PI_4);
                if self.border_style() == BorderStyle::Circle {
                    canvas.set_ring(Some(border::circle_ring(&layout, self.glyphs)));
                }
            }
            canvas
        });
        if self.geometry.layout.is_some() {
            let (width, height) = self.plate_size();
            let shape = self.config.plate.shape;
            self.particles.set_shape(shape);
            self.particles
                .resize_count(particle_count(&self.config, width, height, shape));
            self.particles.configure(&self.config.particles);
            self.cache = FieldCache::for_plate(width, height);
            self.field = ActiveField::for_plate(width, height);
            self.field.set_rotation(self.rotation);
            self.mapper
                .resize(self.config.plate.max_order, width, height);
            self.field
                .set_modes(self.mapper.weights().as_slice(), &mut self.cache)?;
        }
        self.dirty = true;
        Ok(())
    }

    // --- The frame ---------------------------------------------------------

    /// Advance by `dt` seconds. Pass `timings` to measure the stages.
    ///
    /// Returns whether the frame was redrawn. A step that cannot change the
    /// picture skips the simulation, the rasterizing and the composing: the
    /// sand does not move in silence, so a plate at rest costs the analysis
    /// of a quiet signal and nothing more.
    pub fn step(&mut self, dt: f32, mut timings: Option<&mut Timings>) -> Result<bool, FieldError> {
        let after_rest = self.resting;
        self.resting = false;
        let started = timings.is_some().then(Instant::now);
        if let Some(notice) = &mut self.notice {
            self.dirty = true;
            if !notice.tick(dt) {
                self.notice = None;
            }
        }
        if self.paused && !self.dirty {
            return Ok(self.rest(timings, started));
        }
        let Some(layout) = self.geometry.layout else {
            if !self.dirty {
                return Ok(self.rest(timings, started));
            }
            self.frame.message("Terminal too small");
            self.dirty = false;
            return Ok(true);
        };
        let mut watch = Stopwatch::start(timings.is_some());
        if !self.paused {
            self.features = *self.analyzer.analyze(dt);
            self.mapper.update(&self.features, dt);
            self.track_silence(dt);
            if self.is_asleep() != self.shown_asleep {
                self.dirty = true;
            }
            if !self.dirty && self.can_rest() {
                self.advance_rotation(dt);
                // The meter's columns keep their rhythm through the rest.
                self.meter.update(0.0, dt);
                return Ok(self.rest(timings, started));
            }
            self.track_presentation(dt, after_rest);
            watch.lap(timings.as_deref_mut().map(|t| &mut t.audio));
            self.field
                .set_modes(self.mapper.weights().as_slice(), &mut self.cache)?;
            self.advance_rotation(dt);
            watch.lap(timings.as_deref_mut().map(|t| &mut t.field));
            self.particles.step(
                self.field.grid(),
                self.features.energy,
                self.features.onset,
                dt,
                &self.config.particles,
                layout.subpixel_width,
                layout.subpixel_height,
            );
            watch.lap(timings.as_deref_mut().map(|t| &mut t.simulation));
        }
        if let Some(canvas) = &mut self.canvas {
            canvas.rasterize_particles(&self.particles);
            watch.lap(timings.as_deref_mut().map(|t| &mut t.rasterize));
        }
        self.compose();
        watch.lap(timings.as_deref_mut().map(|t| &mut t.glyphs));
        if let Some(timings) = timings {
            timings.frames += 1;
        }
        self.dirty = false;
        Ok(true)
    }

    /// Record a step that drew nothing.
    fn rest(&mut self, timings: Option<&mut Timings>, started: Option<Instant>) -> bool {
        self.resting = true;
        if let (Some(timings), Some(started)) = (timings, started) {
            timings.idle += 1;
            timings.resting += started.elapsed();
        }
        false
    }

    /// The drum turns slowly so that degenerate figures never look frozen.
    /// Wrapping keeps the angle precise over long runs.
    fn advance_rotation(&mut self, dt: f32) {
        if self.config.plate.shape == Shape::Circle {
            self.rotation = (self.rotation + self.config.plate.rotate_speed * dt) % TAU;
            self.field.set_rotation(self.rotation);
        }
    }

    /// Count the time and the frames since the last sound.
    fn track_silence(&mut self, dt: f32) {
        let silent = self.features.energy <= 0.0 && self.features.onset <= 0.0;
        if !silent {
            self.silent_for = 0.0;
            self.quiet_frames = 0;
        } else if dt.is_finite() && dt > 0.0 {
            self.silent_for += dt;
        }
    }

    /// Whether drawing again would give the very same picture.
    fn can_rest(&mut self) -> bool {
        if !self.rest_allowed {
            return false;
        }
        // Asleep, the plate is blank whatever else happens underneath.
        if self.is_asleep() {
            return true;
        }
        let silent = self.features.energy <= 0.0 && self.features.onset <= 0.0;
        if !silent {
            return false;
        }
        if self.quiet_frames < settle_frames(self.config.render.trail) {
            // The first silent frames still have to be drawn: a trail needs
            // them to fade, and the last one shows the sand at rest.
            self.quiet_frames += 1;
            return false;
        }
        // Amplitude colors follow the field, which moves while a drum turns.
        let field_moves = self.palette.colorize() == Colorize::Amplitude
            && self.config.plate.shape == Shape::Circle
            && self.config.plate.rotate_speed != 0.0;
        self.mapper.is_settled()
            && self.meter.is_flat()
            && self.note_tracker.is_idle()
            && self.note_color.is_settled()
            && !field_moves
    }

    /// Update what only the display cares about: the frame rate, the meter,
    /// the last pitch and the note color.
    fn track_presentation(&mut self, dt: f32, after_rest: bool) {
        // The step after a rest spans the whole rest, not one frame.
        if dt.is_finite() && dt > 0.0 && !after_rest {
            let instant = 1.0 / dt;
            self.fps = if self.fps == 0.0 {
                instant
            } else {
                self.fps + (instant - self.fps) * 0.1
            };
        }
        self.meter.update(self.features.energy, dt);
        self.meter_text.clear();
        self.meter
            .write(self.glyphs.is_unicode(), &mut self.meter_text);
        let confident = self
            .features
            .pitch_class
            .filter(|_| self.features.confidence >= self.config.mapping.min_confidence);
        if let (Some(class), Some(hz)) = (confident, self.features.dominant_hz) {
            self.last_note = Some((class, self.features.octave, hz));
        }
        let hold_ms = self.config.mapping.hold_ms as f32;
        if let Some(class) = self.note_tracker.update(confident, hold_ms, dt) {
            self.note_color.change(class);
        }
        self.note_color
            .advance(dt, self.config.mapping.transition_ms as f32);
    }

    /// Paint the whole frame: background, sand, border, status row, help.
    fn compose(&mut self) {
        self.frame.clear();
        if let Some(background) = self.palette.background {
            for cell in self.frame.cells_mut() {
                cell.bg = Some(background);
            }
        }
        // A sleeping plate shows nothing at all, not even the status row;
        // sound, a key or a notice brings everything back.
        self.shown_asleep = self.is_asleep();
        if self.shown_asleep {
            return;
        }
        if let (Some(canvas), Some(layout)) = (&self.canvas, self.geometry.layout) {
            let palette = &self.palette;
            let field = self.field.grid();
            let note = palette.note(
                self.note_color.from,
                self.note_color.to,
                self.note_color.progress,
            );
            let (width, height) = (f32::from(layout.width), f32::from(layout.height));
            let amplitude_used = palette.colorize() == Colorize::Amplitude;
            canvas.write_cells(&mut self.frame, |cell| {
                if cell.ring {
                    return palette.border;
                }
                let x = (f32::from(cell.x) + 0.5) / width;
                let y = (f32::from(cell.y) + 0.5) / height;
                palette.cell(
                    CellInput {
                        density: cell.density,
                        x,
                        y,
                        radius: (2.0 * (x - 0.5).hypot(y - 0.5)).min(1.0),
                        amplitude: if amplitude_used {
                            field.sample(x, y)
                        } else {
                            0.0
                        },
                    },
                    note,
                )
            });
        }
        self.draw_border();
        self.draw_status_row();
        if self.help {
            let chars = border::box_chars(BorderStyle::Rounded, self.glyphs.is_unicode());
            if let Some(chars) = chars {
                hud::draw_help(
                    &mut self.frame,
                    &self.palette,
                    chars,
                    self.glyphs.is_unicode(),
                );
            }
        }
    }

    fn status_values(&self) -> StatusValues<'_> {
        StatusValues {
            note: self.last_note.map(|(class, octave, _)| (class, octave)),
            hz: self.last_note.map(|(_, _, hz)| hz),
            mode: self.mapper.dominant_mode(),
            shape: self.config.plate.shape,
            theme: &self.theme,
            fps: self.fps,
            particles: self.particles.len(),
            source: &self.source,
            energy: &self.meter_text,
        }
    }

    fn draw_border(&mut self) {
        let Some(layout) = self.geometry.layout else {
            return;
        };
        let style = self.border_style();
        let unicode = self.glyphs.is_unicode();
        let mut title_line = std::mem::take(&mut self.title_line);
        let has_title = !self.title_template.is_empty();
        if has_title {
            hud::render_template(&mut title_line, &self.title_template, &self.status_values());
        }
        let title = title_line.text();
        let align = self.config.border.title_align;
        if let (Some(outline), Some(chars)) =
            (self.geometry.outline, border::box_chars(style, unicode))
        {
            border::draw_box(&mut self.frame, outline, chars, self.palette.border);
            if has_title {
                border::draw_box_title(&mut self.frame, outline, title, align, self.palette.title);
            }
        } else if has_title && style == BorderStyle::Circle && layout.y > 0 {
            // The ring has no edge to carry a title: put it on the row above.
            border::draw_aligned(
                &mut self.frame,
                layout.x,
                layout.y - 1,
                layout.width,
                title,
                align,
                self.palette.title,
            );
        }
        self.title_line = title_line;
    }

    fn draw_status_row(&mut self) {
        let (width, height) = self.size;
        if width == 0 || height == 0 {
            return;
        }
        let row = match self.config.hud.position {
            HudPosition::Top => 0,
            HudPosition::Bottom => height - 1,
        };
        let x = self.config.layout.padding[1].min(width - 1);
        let room = width - x;
        let colors = LineColors {
            plain: self.palette.hud_text,
            value: self.palette.hud_accent,
            dim: self.palette.hud_dim,
        };
        if let Some((kind, text)) = self.notice.as_ref().map(|n| (n.kind, n.text.clone())) {
            let color = match kind {
                NoticeKind::Error => self.palette.hud_error,
                NoticeKind::Info => self.palette.hud_accent,
            };
            self.clear_row(row);
            let mut line = TextLine::new();
            line.push(&text, Emphasis::Plain);
            let colors = LineColors {
                plain: color,
                ..colors
            };
            hud::draw_line(&mut self.frame, x, row, room, &line, colors);
        } else if self.config.hud.enabled {
            let mut line = std::mem::take(&mut self.line);
            hud::render_template(&mut line, &self.hud_template, &self.status_values());
            if !self.connected {
                line.push("  no audio", Emphasis::Dim);
            }
            hud::draw_line(&mut self.frame, x, row, room, &line, colors);
            self.line = line;
        }
    }

    /// Blank a row, so that a notice is not mixed with what is under it.
    fn clear_row(&mut self, row: u16) {
        for x in 0..self.size.0 {
            if let Some(cell) = self.frame.cell_mut(x, row) {
                cell.character = ' ';
                cell.fg = None;
            }
        }
    }
}

/// How many silent frames a trail needs to fade into its final picture, so
/// that the afterimage is within a millionth of where it ends up and a later
/// frame cannot differ by more than rounding. A trail of one never settles,
/// so the wait is capped.
fn settle_frames(trail: f32) -> u32 {
    const MIN: u32 = 2;
    const MAX: u32 = 3_600;
    // NaN and zero both mean no afterimage.
    if trail.is_nan() || trail <= 0.0 {
        return MIN;
    }
    let frames = (1.0e-6_f32.ln() / trail.min(0.999_999).ln()).ceil();
    (frames as u32).saturating_add(MIN).min(MAX)
}

fn resolve_glyphs(config: &Config, environment: &Environment) -> GlyphSet {
    GlyphSet::resolve(
        config.render.glyphs,
        environment.locale.as_deref(),
        environment.term.as_deref(),
    )
}

fn make_palette(config: &Config, scheme: &ColorScheme, environment: &Environment) -> Palette {
    Palette::new(
        scheme,
        ColorLevel::resolve(config.render.color_mode, environment),
        config.render.gradient_steps,
    )
}

/// Configuration validation guarantees that templates parse; an engine handed
/// an unvalidated one shows nothing rather than failing.
fn parse_template(text: &str) -> Vec<Segment> {
    template::parse(text).unwrap_or_default()
}

fn hud_shape(shape: Shape) -> &'static str {
    match shape {
        Shape::Square => "square",
        Shape::Circle => "circle",
    }
}

/// Auto counts follow the plate's area: a drum covers pi/4 of its square.
fn particle_count(config: &Config, width: usize, height: usize, shape: Shape) -> usize {
    match config.particles.count {
        ParticleCount::Fixed(count) => count,
        ParticleCount::Auto => {
            let area = match shape {
                Shape::Square => 1.0,
                Shape::Circle => std::f32::consts::FRAC_PI_4,
            };
            ((width as f32 * height as f32 * area * config.particles.density) as usize)
                .clamp(2_000, 150_000)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{
        audio::{SampleSource, demo::DemoSynth},
        config::{
            color::Color,
            schema::{ColorMode, TitleAlign},
        },
    };
    use crossterm::style::Color as TermColor;

    const DT: f32 = 1.0 / 60.0;

    fn environment() -> Environment {
        Environment {
            locale: Some("en_US.UTF-8".to_owned()),
            term: Some("xterm-256color".to_owned()),
            colorterm: Some("truecolor".to_owned()),
            no_color: false,
        }
    }

    fn rgb(hex: &str) -> Color {
        hex.parse().unwrap()
    }

    fn engine_with(
        configure: impl FnOnce(&mut Config),
        recolor: impl FnOnce(&mut ColorScheme),
    ) -> Engine {
        let mut config = Config::default();
        config.particles.count = ParticleCount::Fixed(1_500);
        config.render.glyphs = crate::config::schema::Glyphs::Braille;
        configure(&mut config);
        let mut scheme = ColorScheme::builtin_default();
        recolor(&mut scheme);
        Engine::new(Setup {
            config,
            scheme,
            environment: environment(),
            width: 80,
            height: 40,
            seed: 42,
            sample_rate: 48_000,
        })
        .unwrap()
    }

    fn engine(configure: impl FnOnce(&mut Config)) -> Engine {
        engine_with(configure, |_| {})
    }

    /// Run the demo through the engine for `frames` frames.
    fn run(engine: &mut Engine, synth: &mut DemoSynth, frames: usize) {
        let mut samples = Vec::new();
        for _ in 0..frames {
            samples.clear();
            synth.pull(DT, &mut samples).unwrap();
            engine.feed(&samples);
            engine.step(DT, None).unwrap();
        }
    }

    fn warmed(
        configure: impl FnOnce(&mut Config),
        recolor: impl FnOnce(&mut ColorScheme),
    ) -> Engine {
        let mut engine = engine_with(configure, recolor);
        run(&mut engine, &mut DemoSynth::new(42), 150);
        engine
    }

    fn text(engine: &Engine) -> String {
        engine.frame().plain_text()
    }

    fn row(engine: &Engine, y: u16) -> String {
        text(engine).lines().nth(usize::from(y)).unwrap().to_owned()
    }

    fn fg(engine: &Engine, x: u16, y: u16) -> Option<TermColor> {
        engine.frame().cell(x, y).unwrap().fg
    }

    // --- The pipeline ------------------------------------------------------

    #[test]
    fn the_demo_drives_the_field_through_the_analyzer() {
        let engine = warmed(|_| {}, |_| {});
        let features = engine.features();
        assert!(features.energy > 0.0);
        assert!(features.pitch_class.is_some());
        assert!(!engine.mapper().weights().is_empty());
        assert!(text(&engine).chars().any(|c| c != ' ' && c != '\n'));
    }

    #[test]
    fn identical_input_gives_identical_frames_and_other_seeds_do_not() {
        let render = |seed: u64| {
            let mut config = Config::default();
            config.particles.count = ParticleCount::Fixed(2_000);
            let mut engine = Engine::new(Setup {
                config,
                scheme: ColorScheme::builtin_default(),
                environment: environment(),
                width: 60,
                height: 30,
                seed,
                sample_rate: 48_000,
            })
            .unwrap();
            run(&mut engine, &mut DemoSynth::new(seed), 200);
            engine.frame().ansi_text()
        };
        assert_eq!(render(5), render(5));
        assert_ne!(render(5), render(6));
    }

    #[test]
    fn drum_engines_keep_the_sand_inside_the_circle() {
        let engine = warmed(|config| config.plate.shape = Shape::Circle, |_| {});
        let (px, py, width, height) = engine.plate_rect().unwrap();
        let (cx, cy) = (
            f32::from(px) + f32::from(width) / 2.0,
            f32::from(py) + f32::from(height) / 2.0,
        );
        let frame = engine.frame();
        let mut lit = 0;
        for y in 0..frame.height() {
            for x in 0..frame.width() {
                if frame.cell(x, y).unwrap().character == ' ' {
                    continue;
                }
                lit += 1;
                // Cells are half as wide as tall: scale x to physical units.
                let dx = (f32::from(x) + 0.5 - cx) * 0.5;
                let dy = f32::from(y) + 0.5 - cy;
                let radius = f32::from(height) / 2.0 + 0.6;
                assert!(
                    dx * dx + dy * dy <= radius * radius,
                    "({x}, {y}) lies outside the drum"
                );
            }
        }
        assert!(lit > 50);
        assert!(engine.rotation > 0.0);
    }

    #[test]
    fn steady_frames_and_figure_changes_reuse_buffers() {
        let mut engine = engine(|_| {});
        let mut synth = DemoSynth::new(7);
        run(&mut engine, &mut synth, 400);
        let positions = engine.particles.x().as_ptr();
        let cells = engine.frame.cells().as_ptr();
        let counts = engine.canvas.as_ref().unwrap().counts().as_ptr();
        run(&mut engine, &mut synth, 300);
        assert_eq!(engine.particles.x().as_ptr(), positions);
        assert_eq!(engine.frame.cells().as_ptr(), cells);
        assert_eq!(engine.canvas.as_ref().unwrap().counts().as_ptr(), counts);
    }

    #[test]
    fn freezing_stops_everything_and_drops_audio_until_thawed() {
        let mut engine = engine(|_| {});
        let mut synth = DemoSynth::new(3);
        run(&mut engine, &mut synth, 120);
        engine.toggle_pause();
        engine.step(DT, None).unwrap();
        let frame = engine.frame().clone();
        let x = engine.particles.x().to_vec();
        let weights = *engine.mapper().weights();
        run(&mut engine, &mut synth, 120);
        assert_eq!(*engine.frame(), frame);
        assert_eq!(engine.particles.x(), x);
        assert_eq!(*engine.mapper().weights(), weights);
        engine.toggle_pause();
        run(&mut engine, &mut synth, 30);
        assert_ne!(engine.particles.x(), x);
    }

    #[test]
    fn resize_preserves_sand_and_survives_tiny_terminals() {
        let mut engine = engine(|_| {});
        let mut synth = DemoSynth::new(9);
        run(&mut engine, &mut synth, 100);
        engine.toggle_pause();
        let x = engine.particles.x().to_vec();
        engine.resize(100, 50).unwrap();
        engine.step(DT, None).unwrap();
        assert_eq!(engine.particles.x(), x);
        engine.resize(3, 2).unwrap();
        engine.step(DT, None).unwrap();
        assert!(!engine.has_plate());
        assert!(!text(&engine).trim().is_empty());
        engine.resize(80, 40).unwrap();
        engine.step(DT, None).unwrap();
        assert_eq!(engine.particles.x(), x);
        engine.toggle_pause();
        run(&mut engine, &mut synth, 10);
    }

    #[test]
    fn silence_leaves_the_sand_and_the_figure_alone() {
        let mut engine = engine(|_| {});
        let mut synth = DemoSynth::new(2);
        run(&mut engine, &mut synth, 200);
        for _ in 0..240 {
            engine.feed(&[0.0; 800]);
            engine.step(DT, None).unwrap();
        }
        let weights = *engine.mapper().weights();
        for _ in 0..600 {
            engine.feed(&[0.0; 800]);
            engine.step(DT, None).unwrap();
        }
        let x = engine.particles.x().to_vec();
        for _ in 0..60 {
            engine.feed(&[0.0; 800]);
            engine.step(DT, None).unwrap();
        }
        assert_eq!(engine.features().energy, 0.0);
        assert_eq!(*engine.mapper().weights(), weights);
        assert_eq!(engine.particles.x(), x);
    }

    #[test]
    fn auto_counts_follow_the_plate_area() {
        let config = Config::default();
        let square = particle_count(&config, 152, 152, Shape::Square);
        let drum = particle_count(&config, 152, 152, Shape::Circle);
        assert_eq!(square, (152.0 * 152.0 * 0.45) as usize);
        assert!((drum as f32 / square as f32 - 0.785).abs() < 0.01);
        assert_eq!(particle_count(&config, 1, 1, Shape::Square), 2_000);
        assert_eq!(
            particle_count(&config, 4_000, 4_000, Shape::Square),
            150_000
        );
    }

    // --- Colors ------------------------------------------------------------

    fn light_gradient(scheme: &mut ColorScheme) {
        scheme.gradient = vec![rgb("#102030"), rgb("#f0e0d0")];
    }

    #[test]
    fn sand_cells_carry_gradient_colors_and_empty_cells_stay_plain() {
        let engine = warmed(
            |config| config.particles.count = ParticleCount::Fixed(8_000),
            light_gradient,
        );
        let frame = engine.frame();
        let mut colors = std::collections::BTreeSet::new();
        for y in 0..frame.height() {
            for x in 0..frame.width() {
                let cell = frame.cell(x, y).unwrap();
                assert_eq!(cell.bg, None, "the background is never painted by default");
                if cell.character == ' ' {
                    assert_eq!(cell.fg, None);
                } else {
                    let Some(TermColor::Rgb { r, g, b }) = cell.fg else {
                        panic!("a sand cell needs an RGB color at truecolor: {cell:?}");
                    };
                    colors.insert((r, g, b));
                }
            }
        }
        assert!(colors.len() >= 3, "density shades the sand: {colors:?}");
        // The quantization keeps the palette small.
        assert!(colors.len() <= engine.config().render.gradient_steps);
    }

    #[test]
    fn the_background_is_painted_only_when_the_scheme_sets_one() {
        let painted = warmed(|_| {}, |scheme| scheme.background = rgb("#101820"));
        assert!(painted.frame().cells().iter().all(|cell| cell.bg
            == Some(TermColor::Rgb {
                r: 0x10,
                g: 0x18,
                b: 0x20
            })));
        // Sand keeps the background it sits on.
        assert!(
            painted
                .frame()
                .cells()
                .iter()
                .any(|c| c.character != ' ' && c.bg.is_some())
        );
        let transparent = warmed(|_| {}, |_| {});
        assert!(
            transparent
                .frame()
                .cells()
                .iter()
                .all(|cell| cell.bg.is_none())
        );
        let plain = warmed(|_| {}, |scheme| scheme.background = Color::Default);
        assert!(plain.frame().cells().iter().all(|cell| cell.bg.is_none()));
    }

    #[test]
    fn color_mode_none_paints_nothing_and_lower_levels_quantize() {
        let none = warmed(
            |config| config.render.color_mode = ColorMode::None,
            light_gradient,
        );
        assert!(
            none.frame()
                .cells()
                .iter()
                .all(|c| c.fg.is_none() && c.bg.is_none())
        );
        assert_eq!(none.frame().ansi_text(), none.frame().plain_text());
        let ansi256 = warmed(
            |config| config.render.color_mode = ColorMode::Ansi256,
            light_gradient,
        );
        assert!(
            ansi256
                .frame()
                .cells()
                .iter()
                .filter(|c| c.character != ' ')
                .all(|c| matches!(c.fg, Some(TermColor::AnsiValue(index)) if index >= 16))
        );
        let ansi16 = warmed(
            |config| config.render.color_mode = ColorMode::Ansi16,
            light_gradient,
        );
        assert!(
            ansi16
                .frame()
                .cells()
                .iter()
                .filter(|c| c.character != ' ')
                .all(|c| matches!(c.fg, Some(TermColor::AnsiValue(index)) if index < 16))
        );
        // NO_COLOR decides `auto`, but not an explicit mode.
        let mut environment = environment();
        environment.no_color = true;
        let level = |mode| ColorLevel::resolve(mode, &environment);
        assert_eq!(level(ColorMode::Auto), ColorLevel::None);
        assert_eq!(level(ColorMode::Truecolor), ColorLevel::TrueColor);
    }

    fn color_map(engine: &Engine) -> Vec<Option<TermColor>> {
        engine.frame().cells().iter().map(|cell| cell.fg).collect()
    }

    #[test]
    fn each_colorize_mode_gives_the_sand_its_own_colors() {
        let build = |mode: Colorize| {
            warmed(
                |config| config.colors.colorize = Some(mode),
                |scheme| {
                    light_gradient(scheme);
                    scheme.sand = rgb("#44aa88");
                    scheme.colorize = mode;
                },
            )
        };
        let modes = [
            Colorize::Solid,
            Colorize::Density,
            Colorize::Radial,
            Colorize::Vertical,
            Colorize::Horizontal,
            Colorize::Amplitude,
            Colorize::Note,
        ];
        let engines: Vec<_> = modes.iter().map(|&mode| build(mode)).collect();
        // The sand is the same in every mode (same seed, same input) ...
        for engine in &engines {
            assert_eq!(text(engine), text(&engines[0]));
        }
        // ... and only the colors differ, in every pair of modes.
        for (index, first) in engines.iter().enumerate() {
            for (other, second) in engines.iter().enumerate().skip(index + 1) {
                assert_ne!(
                    color_map(first),
                    color_map(second),
                    "{:?} and {:?} look the same",
                    modes[index],
                    modes[other]
                );
            }
        }
        // Solid is one color; vertical grows with the row.
        let distinct = |engine: &Engine| {
            let mut colors: Vec<_> = color_map(engine).into_iter().flatten().collect();
            colors.dedup();
            let unique: std::collections::HashSet<_> =
                colors.iter().map(|c| format!("{c:?}")).collect();
            unique.len()
        };
        assert_eq!(distinct(&engines[0]), 1);
        assert!(distinct(&engines[3]) > 5);
        let vertical = &engines[3];
        let lightness = |y: u16| {
            let mut sum = 0.0;
            let mut count: f32 = 0.0;
            for x in 0..vertical.frame().width() {
                if let Some(TermColor::Rgb { r, .. }) = fg(vertical, x, y) {
                    sum += f32::from(r);
                    count += 1.0;
                }
            }
            sum / count.max(1.0)
        };
        let (px, py, _, height) = vertical.plate_rect().unwrap();
        assert!(px > 0);
        assert!(lightness(py + 2) < lightness(py + height - 3));
    }

    #[test]
    fn note_mode_colors_all_sand_by_the_note_and_fades_when_it_changes() {
        let scheme_colors = |scheme: &mut ColorScheme| {
            scheme.colorize = Colorize::Note;
            for (index, color) in scheme.note_colors.iter_mut().enumerate() {
                let level = (index * 20) as u8;
                *color = Color::Rgb {
                    r: level,
                    g: 255 - level,
                    b: 128,
                };
            }
        };
        let mut engine = engine_with(|config| config.mapping.transition_ms = 400, scheme_colors);
        let mut synth = DemoSynth::new(42);
        let sand_colors = |engine: &Engine| {
            let mut colors: Vec<_> = color_map(engine).into_iter().flatten().collect();
            colors.sort_by_key(|color| format!("{color:?}"));
            colors.dedup();
            colors
        };
        // Before any note the plate shows the sand color; a note then takes over.
        run(&mut engine, &mut synth, 60);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..30 {
            run(&mut engine, &mut synth, 10);
            let colors = sand_colors(&engine);
            assert_eq!(colors.len(), 1, "one note, one color: {colors:?}");
            seen.insert(format!("{:?}", colors[0]));
        }
        assert!(seen.len() >= 4, "the melody changes the color: {seen:?}");
        // While fading, the color goes through in-between values.
        let mut in_between = false;
        let mut engine = engine_with(|config| config.mapping.transition_ms = 600, scheme_colors);
        let mut synth = DemoSynth::new(42);
        let mut previous = String::new();
        let mut steps_at_a_color = 0;
        for _ in 0..600 {
            run(&mut engine, &mut synth, 1);
            let colors = sand_colors(&engine);
            if colors.len() == 1 {
                let now = format!("{:?}", colors[0]);
                if now != previous {
                    steps_at_a_color += 1;
                    previous = now;
                }
            }
            in_between |= steps_at_a_color > 12;
        }
        assert!(
            in_between,
            "colors change gradually, not in {steps_at_a_color} jumps"
        );
    }

    #[test]
    fn amplitude_mode_samples_the_field_and_other_modes_do_not_need_to() {
        let engine = warmed(
            |_| {},
            |scheme| {
                scheme.colorize = Colorize::Amplitude;
                light_gradient(scheme);
            },
        );
        assert_eq!(engine.palette().colorize(), Colorize::Amplitude);
        assert!(color_map(&engine).into_iter().flatten().count() > 20);
    }

    // --- Border ------------------------------------------------------------

    #[test]
    fn a_box_border_surrounds_the_plate_and_takes_its_color() {
        let engine = warmed(
            |config| config.border.style = BorderStyle::Rounded,
            |scheme| scheme.border = rgb("#336699"),
        );
        let (x, y, width, height) = engine.plate_rect().unwrap();
        let color = Some(TermColor::Rgb {
            r: 0x33,
            g: 0x66,
            b: 0x99,
        });
        let corner = |cx: u16, cy: u16| engine.frame().cell(cx, cy).unwrap().character;
        assert_eq!(corner(x - 1, y - 1), '╭');
        assert_eq!(corner(x + width, y - 1), '╮');
        assert_eq!(corner(x - 1, y + height), '╰');
        assert_eq!(corner(x + width, y + height), '╯');
        assert_eq!(fg(&engine, x - 1, y - 1), color);
        assert_eq!(corner(x + 3, y - 1), '─');
        assert_eq!(corner(x - 1, y + 3), '│');
        // The plate shrank to make room: it has no cell in the padding ring.
        let without = warmed(|_| {}, |_| {});
        let (_, _, plain_width, plain_height) = without.plate_rect().unwrap();
        assert!(width < plain_width && height < plain_height);
        assert!(
            x >= 3 && y >= 2,
            "box stays inside the padding: plate at {x},{y}"
        );
        // Other styles use their own characters.
        for (style, top_left) in [
            (BorderStyle::Square, '┌'),
            (BorderStyle::Double, '╔'),
            (BorderStyle::Thick, '┏'),
        ] {
            let engine = warmed(|config| config.border.style = style, |_| {});
            let (x, y, ..) = engine.plate_rect().unwrap();
            assert_eq!(
                engine.frame().cell(x - 1, y - 1).unwrap().character,
                top_left
            );
        }
    }

    #[test]
    fn a_title_is_written_into_the_border_with_its_own_color() {
        let engine = warmed(
            |config| {
                config.border.style = BorderStyle::Square;
                config.border.title = " {shape} ".to_owned();
                config.border.title_align = TitleAlign::Left;
            },
            |scheme| scheme.title = rgb("#ff8800"),
        );
        let (x, y, ..) = engine.plate_rect().unwrap();
        let top = row(&engine, y - 1);
        assert!(top.contains(" square "), "{top}");
        let start = top.chars().position(|c| c == 's').unwrap() as u16;
        assert_eq!(
            start,
            x + 2,
            "left aligned after the corner, one line cell and a space"
        );
        assert_eq!(
            fg(&engine, start, y - 1),
            Some(TermColor::Rgb {
                r: 255,
                g: 136,
                b: 0
            })
        );
        // The placeholders are live: the note follows the music.
        let engine = warmed(
            |config| {
                config.border.style = BorderStyle::Rounded;
                config.border.title = "{note}{octave}".to_owned();
            },
            |_| {},
        );
        let (_, y, ..) = engine.plate_rect().unwrap();
        let top = row(&engine, y - 1);
        assert!(top.chars().any(|c| "ABCDEFG".contains(c)), "{top}");
    }

    #[test]
    fn an_ascii_terminal_gets_an_ascii_box() {
        let engine = warmed(
            |config| {
                config.border.style = BorderStyle::Double;
                config.render.glyphs = crate::config::schema::Glyphs::Ascii;
            },
            |_| {},
        );
        let (x, y, ..) = engine.plate_rect().unwrap();
        assert_eq!(engine.frame().cell(x - 1, y - 1).unwrap().character, '+');
        assert_eq!(engine.frame().cell(x + 2, y - 1).unwrap().character, '-');
        assert!(text(&engine).is_ascii());
    }

    #[test]
    fn a_drum_gets_a_ring_of_the_plates_own_glyphs_and_a_square_plate_a_box() {
        let engine = warmed(
            |config| {
                config.plate.shape = Shape::Circle;
                config.border.style = BorderStyle::Circle;
                config.border.title = "drum".to_owned();
            },
            |scheme| scheme.border = rgb("#00ff00"),
        );
        let (x, y, width, height) = engine.plate_rect().unwrap();
        let green = Some(TermColor::Rgb { r: 0, g: 255, b: 0 });
        // The ring touches the middle of every side, and its glyphs are braille.
        for (cx, cy) in [
            (x + width / 2, y),
            (x + width / 2, y + height - 1),
            (x, y + height / 2),
            (x + width - 1, y + height / 2),
        ] {
            let cell = engine.frame().cell(cx, cy).unwrap();
            assert!(
                ('\u{2801}'..='\u{28ff}').contains(&cell.character),
                "{cell:?}"
            );
            assert_eq!(cell.fg, green);
        }
        // No box edges, and the corners of the square stay empty.
        assert_eq!(engine.frame().cell(x - 1, y - 1).unwrap().character, ' ');
        assert_eq!(engine.frame().cell(x, y).unwrap().character, ' ');
        // The title goes on the row above the plate.
        assert!(row(&engine, y - 1).contains("drum"));
        // A circle border has no meaning on a square plate: a box takes over.
        let square = warmed(|config| config.border.style = BorderStyle::Circle, |_| {});
        let (x, y, ..) = square.plate_rect().unwrap();
        assert_eq!(square.frame().cell(x - 1, y - 1).unwrap().character, '╭');
    }

    // --- Glyph sets -------------------------------------------------------

    #[test]
    fn every_glyph_set_draws_with_its_own_characters_on_the_same_plate_size() {
        use crate::config::schema::Glyphs;
        let draw = |glyphs: Glyphs| warmed(|config| config.render.glyphs = glyphs, |_| {});
        let braille = draw(Glyphs::Braille);
        let quadrant = draw(Glyphs::Quadrant);
        let half = draw(Glyphs::Half);
        let ascii = draw(Glyphs::Ascii);
        let used = |engine: &Engine| {
            text(engine)
                .chars()
                .filter(|c| *c != ' ' && *c != '\n')
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert!(
            used(&braille)
                .iter()
                .all(|c| ('\u{2801}'..='\u{28ff}').contains(c))
        );
        assert!(
            used(&quadrant)
                .iter()
                .all(|c| " ▘▝▀▖▌▞▛▗▚▐▜▄▙▟█".contains(*c))
        );
        assert!(used(&half).iter().all(|c| "▀▄█".contains(*c)));
        assert!(used(&ascii).iter().all(|c| c.is_ascii_graphic()));
        assert!(used(&half).len() >= 2 && used(&quadrant).len() >= 4);
        // The plate is the same physical square whatever its resolution.
        let size = |engine: &Engine| engine.plate_rect().map(|(_, _, w, h)| (w, h));
        assert_eq!(size(&braille), size(&quadrant));
        assert_eq!(size(&braille), size(&half));
        assert_eq!(size(&braille), size(&ascii));
    }

    // --- Status row ----------------------------------------------------------

    #[test]
    fn the_hud_reserves_its_row_and_shows_the_music() {
        let off = warmed(|_| {}, |_| {});
        let on = warmed(
            |config| {
                config.hud.enabled = true;
                config.hud.format =
                    "{note}{octave} {mode} {shape} {theme} {source} {particles}".to_owned();
            },
            |_| {},
        );
        // One row fewer for the plate, and the status in the last row.
        let (_, _, _, off_height) = off.plate_rect().unwrap();
        let (_, y, _, on_height) = on.plate_rect().unwrap();
        assert!(on_height < off_height, "{on_height} vs {off_height}");
        assert!(y + on_height <= 39);
        let hud = row(&on, 39);
        assert!(hud.trim().len() > 5, "{hud:?}");
        assert!(hud.contains(" square default "), "{hud}");
        assert!(hud.trim_end().ends_with("1500"), "{hud}");
        assert_eq!(row(&off, 39).trim(), "");
        // Values are accented, words use the text color.
        let hud_row = |engine: &Engine| (0..80).map(|x| fg(engine, x, 39)).collect::<Vec<_>>();
        assert!(hud_row(&on).iter().any(Option::is_some));
    }

    #[test]
    fn the_hud_can_sit_at_the_top_and_names_theme_and_source() {
        let mut engine = engine(|config| {
            config.hud.enabled = true;
            config.hud.position = HudPosition::Top;
            config.hud.format = "{theme}|{source}|{fps}".to_owned();
        });
        engine.set_labels("nord", "pulse: speakers");
        let mut synth = DemoSynth::new(1);
        run(&mut engine, &mut synth, 20);
        assert!(
            row(&engine, 0).starts_with("  nord|pulse: speakers|60"),
            "{}",
            row(&engine, 0)
        );
        assert_eq!(row(&engine, 39).trim(), "");
        let (_, y, ..) = engine.plate_rect().unwrap();
        assert!(y >= 1, "the plate starts below the status row");
        // The frame rate follows the time step actually taken.
        for _ in 0..200 {
            engine.step(1.0 / 30.0, None).unwrap();
        }
        assert!(row(&engine, 0).contains("|30"), "{}", row(&engine, 0));
    }

    #[test]
    fn the_energy_meter_and_the_hz_placeholder_move_with_the_sound() {
        let mut engine = engine(|config| {
            config.hud.enabled = true;
            config.hud.format = "{hz}Hz {energy}".to_owned();
        });
        let mut synth = DemoSynth::new(1);
        let silent = row(&engine, 39);
        assert!(!silent.contains("Hz"));
        run(&mut engine, &mut synth, 120);
        let hud = row(&engine, 39);
        assert!(hud.contains("Hz"), "{hud}");
        let meter: String = hud.chars().filter(|c| "▁▂▃▄▅▆▇█".contains(*c)).collect();
        assert_eq!(meter.chars().count(), EnergyMeter::WIDTH, "{hud}");
        assert!(meter.chars().any(|c| c != '▁'));
        // Before any pitch is heard the plate says so with dashes.
        let mut fresh = engine_with(
            |config| {
                config.hud.enabled = true;
                config.hud.format = "{note}{octave}|{hz}".to_owned();
            },
            |_| {},
        );
        fresh.step(DT, None).unwrap();
        assert!(row(&fresh, 39).contains("-|-"), "{}", row(&fresh, 39));
    }

    #[test]
    fn notices_cover_the_status_row_with_their_color_and_then_expire() {
        let mut engine = engine_with(
            |config| config.hud.enabled = true,
            |scheme| {
                scheme.hud_error = rgb("#ff0000");
                scheme.hud_accent = rgb("#00ff00");
            },
        );
        let mut synth = DemoSynth::new(1);
        run(&mut engine, &mut synth, 10);
        engine.notify(NoticeKind::Error, "reload failed: bad value");
        engine.step(DT, None).unwrap();
        assert!(row(&engine, 39).contains("reload failed: bad value"));
        assert_eq!(
            fg(&engine, 2, 39),
            Some(TermColor::Rgb { r: 255, g: 0, b: 0 })
        );
        engine.notify(NoticeKind::Info, "theme: nord");
        engine.step(DT, None).unwrap();
        assert_eq!(
            fg(&engine, 2, 39),
            Some(TermColor::Rgb { r: 0, g: 255, b: 0 })
        );
        assert!(
            !row(&engine, 39).contains("reload failed"),
            "a new notice replaces the old"
        );
        // After a few seconds the normal status returns.
        run(&mut engine, &mut synth, 300);
        assert!(engine.notice().is_none());
        assert!(!row(&engine, 39).contains("theme: nord"));
        assert!(row(&engine, 39).trim().len() > 3, "the HUD is back");
        // Without a HUD a notice still appears, on the bottom row, and goes.
        let mut quiet = engine_with(|_| {}, |_| {});
        quiet.notify(NoticeKind::Info, "hello");
        quiet.step(DT, None).unwrap();
        assert!(row(&quiet, 39).contains("hello"));
        for _ in 0..300 {
            quiet.step(DT, None).unwrap();
        }
        assert_eq!(row(&quiet, 39).trim(), "");
    }

    #[test]
    fn a_lost_input_is_announced_and_the_hud_says_no_audio_until_it_returns() {
        let mut engine = engine(|config| config.hud.enabled = true);
        let mut synth = DemoSynth::new(1);
        run(&mut engine, &mut synth, 30);
        engine.set_connected(false);
        engine.step(DT, None).unwrap();
        assert!(row(&engine, 39).contains("audio input lost"));
        run(&mut engine, &mut synth, 300);
        assert!(
            row(&engine, 39).contains("no audio"),
            "{}",
            row(&engine, 39)
        );
        engine.set_connected(false);
        assert!(
            engine.notice().is_none(),
            "no repeated alarm for the same loss"
        );
        engine.set_connected(true);
        engine.step(DT, None).unwrap();
        assert!(row(&engine, 39).contains("audio input restored"));
        run(&mut engine, &mut synth, 300);
        assert!(!row(&engine, 39).contains("no audio"));
    }

    #[test]
    fn the_help_overlay_covers_the_plate_and_asks_for_room() {
        let mut engine = warmed(|_| {}, |_| {});
        engine.toggle_help();
        engine.step(DT, None).unwrap();
        assert!(engine.help_open());
        let shown = text(&engine);
        for key in ["grava keys", "sensitivity", "reload the colors only"] {
            assert!(shown.contains(key), "{key}");
        }
        engine.close_help();
        engine.step(DT, None).unwrap();
        assert!(!text(&engine).contains("grava keys"));
        // A terminal too narrow for the box says so instead.
        let mut narrow = Engine::new(Setup {
            config: Config::default(),
            scheme: ColorScheme::builtin_default(),
            environment: environment(),
            width: 30,
            height: 40,
            seed: 1,
            sample_rate: 48_000,
        })
        .unwrap();
        narrow.toggle_help();
        assert!(!narrow.help_open());
        assert!(narrow.notice().unwrap().text.contains("too narrow"));
    }

    // --- Changing a running engine --------------------------------------

    #[test]
    fn toggling_the_hud_makes_or_gives_back_a_row_without_moving_the_sand() {
        let mut engine = warmed(|_| {}, |_| {});
        let before = engine.particles.x().to_vec();
        let (_, _, _, height) = engine.plate_rect().unwrap();
        engine.toggle_hud().unwrap();
        let (_, _, _, with_hud) = engine.plate_rect().unwrap();
        assert!(with_hud < height);
        assert_eq!(engine.particles.x(), before, "grains keep their places");
        engine.toggle_hud().unwrap();
        assert_eq!(engine.plate_rect().unwrap().3, height);
    }

    #[test]
    fn the_plate_toggles_between_square_and_drum_and_the_sand_follows() {
        let mut engine = warmed(|_| {}, |_| {});
        engine.toggle_shape().unwrap();
        assert_eq!(engine.config().plate.shape, Shape::Circle);
        assert_eq!(engine.notice().unwrap().text, "circle plate");
        let mut synth = DemoSynth::new(8);
        run(&mut engine, &mut synth, 200);
        assert!(
            engine
                .particles
                .x()
                .iter()
                .zip(engine.particles.y())
                .all(|(&x, &y)| (x - 0.5).powi(2) + (y - 0.5).powi(2) <= 0.2501)
        );
        // The drum has its own catalog: the figure on show is a drum mode.
        let shown = engine.mapper().dominant_mode().unwrap();
        assert_eq!(shown.shape(), Shape::Circle);
        engine.toggle_shape().unwrap();
        assert_eq!(engine.config().plate.shape, Shape::Square);
        run(&mut engine, &mut synth, 200);
        assert_eq!(
            engine.mapper().dominant_mode().unwrap().shape(),
            Shape::Square
        );
    }

    #[test]
    fn the_mapping_and_glyph_keys_change_the_engine_and_say_so() {
        let mut engine = warmed(|_| {}, |_| {});
        for expected in ["frequency", "spectrum", "note"] {
            engine.cycle_mapping().unwrap();
            assert_eq!(
                engine.notice().unwrap().text,
                format!("mapping: {expected}")
            );
        }
        let count = engine.particle_count();
        let mut names = Vec::new();
        for _ in 0..4 {
            engine.cycle_glyphs().unwrap();
            names.push(engine.glyphs().name());
            engine.step(DT, None).unwrap();
        }
        assert_eq!(names, ["quadrant", "half", "ascii", "braille"]);
        assert_eq!(
            engine.particle_count(),
            count,
            "explicit counts do not change"
        );
        // Automatic counts follow the sub-pixel area of the glyph set.
        let mut auto = Engine::new(Setup {
            config: Config::default(),
            scheme: ColorScheme::builtin_default(),
            environment: environment(),
            width: 80,
            height: 40,
            seed: 1,
            sample_rate: 48_000,
        })
        .unwrap();
        let braille = auto.particle_count();
        auto.cycle_glyphs().unwrap();
        auto.cycle_glyphs().unwrap();
        auto.cycle_glyphs().unwrap();
        assert_eq!(auto.glyphs().name(), "ascii");
        assert!(
            auto.particle_count() < braille / 2,
            "{} vs {braille}",
            auto.particle_count()
        );
    }

    #[test]
    fn sensitivity_moves_in_tenths_within_zero_and_ten() {
        let mut engine = engine(|_| {});
        assert_eq!(engine.adjust_sensitivity(0.1), 1.1);
        assert_eq!(engine.adjust_sensitivity(0.1), 1.2);
        for _ in 0..30 {
            engine.adjust_sensitivity(-0.1);
        }
        assert_eq!(engine.config().general.sensitivity, 0.0);
        for _ in 0..200 {
            engine.adjust_sensitivity(0.1);
        }
        assert_eq!(engine.config().general.sensitivity, 10.0);
        // The analyzer really follows: at zero sensitivity there is no energy.
        engine.adjust_sensitivity(-100.0);
        let mut synth = DemoSynth::new(1);
        run(&mut engine, &mut synth, 60);
        assert_eq!(engine.features().energy, 0.0);
    }

    #[test]
    fn reconfiguring_applies_only_what_changed_and_keeps_the_figure() {
        let mut engine = warmed(|_| {}, |_| {});
        let figure = *engine.mapper().weights();
        let sand = engine.particles.x().to_vec();
        let mut config = engine.config().clone();
        config.hud.enabled = true;
        config.colors.sand = Some(rgb("#abcdef"));
        config.border.title = "x".to_owned();
        engine.reconfigure(config).unwrap();
        assert_eq!(*engine.mapper().weights(), figure);
        assert_eq!(engine.particles.x(), sand);
        // An analysis change restarts the analyzer but not the sand.
        let mut config = engine.config().clone();
        config.analysis.fft_size = 2048;
        engine.reconfigure(config).unwrap();
        assert_eq!(engine.particles.x(), sand);
        let mut synth = DemoSynth::new(42);
        run(&mut engine, &mut synth, 60);
        assert!(engine.features().energy > 0.0);
        // A broken template cannot reach the engine through validation, and
        // an engine handed one anyway shows no status text instead of failing.
        let mut config = engine.config().clone();
        config.hud.format = "{nonsense".to_owned();
        engine.reconfigure(config).unwrap();
        engine.step(DT, None).unwrap();
        assert_eq!(row(&engine, 39).trim(), "");
    }

    #[test]
    fn set_colors_swaps_the_scheme_and_the_color_level_without_a_relayout() {
        let mut engine = warmed(|_| {}, light_gradient);
        let sand = engine.particles.x().to_vec();
        let mut config = engine.config().clone();
        config.render.color_mode = ColorMode::None;
        let mut scheme = ColorScheme::builtin_default();
        scheme.sand = rgb("#112233");
        engine.set_colors(scheme, &config);
        engine.step(DT, None).unwrap();
        assert_eq!(engine.palette().level(), ColorLevel::None);
        assert_eq!(engine.config().render.color_mode, ColorMode::None);
        assert!(engine.frame().cells().iter().all(|cell| cell.fg.is_none()));
        assert_eq!(engine.particles.x().len(), sand.len());
    }

    #[test]
    fn the_note_tracker_needs_the_hold_time_and_ignores_blips() {
        let mut tracker = NoteTracker::default();
        // 100 ms of A is shorter than a 120 ms hold.
        for _ in 0..6 {
            assert_eq!(tracker.update(Some(9), 120.0, DT), None);
        }
        assert_eq!(tracker.update(None, 120.0, DT), None);
        for _ in 0..6 {
            assert_eq!(tracker.update(Some(9), 120.0, DT), None);
        }
        // A sustained note is announced once, then not again.
        let announced: Vec<_> = (0..20)
            .filter_map(|_| tracker.update(Some(9), 120.0, DT))
            .collect();
        assert_eq!(announced, [9]);
        for _ in 0..30 {
            assert_eq!(tracker.update(Some(9), 120.0, DT), None);
        }
        // A new note must itself hold, and a flicker back resets the clock.
        assert_eq!(tracker.update(Some(2), 120.0, DT), None);
        assert_eq!(tracker.update(Some(5), 120.0, DT), None);
        assert_eq!(tracker.update(Some(2), 0.0, DT), Some(2));
        // With no hold time every change is immediate.
        assert_eq!(tracker.update(Some(7), 0.0, DT), Some(7));
    }

    #[test]
    fn the_note_color_fades_over_the_transition_and_restarts_from_the_nearer_side() {
        let mut color = NoteColor {
            from: None,
            to: None,
            progress: 1.0,
        };
        color.change(3);
        assert_eq!((color.from, color.to, color.progress), (None, Some(3), 0.0));
        color.advance(0.1, 400.0);
        assert!((color.progress - 0.25).abs() < 1e-6);
        // Interrupted early: fade on from where it started.
        color.change(5);
        assert_eq!((color.from, color.to), (None, Some(5)));
        color.advance(0.3, 400.0);
        // Interrupted late: fade on from the note nearly reached.
        color.advance(0.1, 400.0);
        color.change(8);
        assert_eq!((color.from, color.to), (Some(5), Some(8)));
        // No transition time means no fade at all.
        color.advance(0.001, 0.0);
        assert_eq!(color.progress, 1.0);
        color.advance(f32::NAN, 400.0);
    }

    // --- Resting and sleeping ---------------------------------------------

    /// One frame of audio: from the synth, or silence.
    fn frame_audio(synth: Option<&mut DemoSynth>) -> Vec<f32> {
        let mut samples = Vec::new();
        match synth {
            Some(synth) => synth.pull(DT, &mut samples).unwrap(),
            None => samples.resize(800, 0.0),
        }
        samples
    }

    fn drive(engine: &mut Engine, audio: &[f32]) -> bool {
        engine.feed(audio);
        engine.step(DT, None).unwrap()
    }

    /// Music, a long silence, and music again.
    fn script() -> Vec<Vec<f32>> {
        let mut synth = DemoSynth::new(42);
        let mut frames: Vec<_> = (0..120).map(|_| frame_audio(Some(&mut synth))).collect();
        frames.extend((0..700).map(|_| frame_audio(None)));
        frames.extend((0..90).map(|_| frame_audio(Some(&mut synth))));
        frames
    }

    #[test]
    fn the_linux_console_gets_ascii_sixteen_colors_and_an_ascii_status_row() {
        let console = Environment {
            locale: Some("en_US.UTF-8".to_owned()),
            term: Some("linux".to_owned()),
            colorterm: None,
            no_color: false,
        };
        let mut config = Config::default();
        config.particles.count = ParticleCount::Fixed(3_000);
        config.hud.enabled = true;
        config.hud.format = "{note}{octave} {mode} {energy}".to_owned();
        config.border.style = BorderStyle::Rounded;
        let mut scheme = ColorScheme::builtin_default();
        light_gradient(&mut scheme);
        let mut engine = Engine::new(Setup {
            config,
            scheme,
            environment: console,
            width: 80,
            height: 40,
            seed: 42,
            sample_rate: 48_000,
        })
        .unwrap();
        run(&mut engine, &mut DemoSynth::new(42), 150);
        assert_eq!(engine.glyphs().name(), "ascii");
        assert!(
            text(&engine).is_ascii(),
            "the console font has no such glyphs"
        );
        assert!(text(&engine).contains('+'), "an ASCII box");
        assert!(row(&engine, 39).chars().any(|c| ".:-=+*#@".contains(c)));
        let colors: Vec<_> = color_map(&engine).into_iter().flatten().collect();
        assert!(!colors.is_empty());
        assert!(
            colors
                .iter()
                .all(|color| matches!(color, TermColor::AnsiValue(index) if *index < 16))
        );
    }

    #[test]
    fn a_plate_at_rest_does_nothing_and_keeps_its_picture() {
        let mut engine = warmed(|_| {}, |_| {});
        let mut timings = Timings::default();
        let mut last = engine.frame().clone();
        let mut drawn = 0;
        for _ in 0..300 {
            engine.feed(&[0.0; 800]);
            if engine.step(DT, Some(&mut timings)).unwrap() {
                drawn += 1;
                last = engine.frame().clone();
            }
            assert_eq!(*engine.frame(), last);
        }
        assert!(engine.is_resting());
        // The sound fades out over a second or so, then everything rests.
        assert!(drawn < 150, "{drawn} frames drawn in silence");
        assert_eq!(timings.frames, drawn);
        assert_eq!(timings.frames + timings.idle, 300);
        assert!(timings.idle > 150);
        // Sound ends the rest at once, and the sand moves again.
        let sand = engine.particles.x().to_vec();
        let mut synth = DemoSynth::new(42);
        let mut woke = false;
        for _ in 0..60 {
            woke |= drive(&mut engine, &frame_audio(Some(&mut synth)));
        }
        assert!(woke && !engine.is_resting());
        assert_ne!(engine.particles.x(), sand);
    }

    #[test]
    fn resting_never_changes_what_the_full_pipeline_would_draw() {
        type Configure = fn(&mut Config);
        struct Case {
            name: &'static str,
            configure: Configure,
            colorize: Colorize,
            rests: bool,
        }
        let case = |name, configure: Configure, colorize, rests| Case {
            name,
            configure,
            colorize,
            rests,
        };
        let cases = [
            case("defaults", |_| {}, Colorize::Density, true),
            case(
                "a trail",
                |config| config.render.trail = 0.95,
                Colorize::Density,
                true,
            ),
            case(
                "a status row, a border and a title",
                |config| {
                    config.hud.enabled = true;
                    config.hud.format = "{note}{octave} {mode} {energy} {particles}".to_owned();
                    config.border.style = BorderStyle::Rounded;
                    config.border.title = "{shape} {theme}".to_owned();
                },
                Colorize::Density,
                true,
            ),
            case(
                "a turning drum",
                |config| config.plate.shape = Shape::Circle,
                Colorize::Density,
                true,
            ),
            case(
                "momentum and kicks",
                |config| {
                    config.particles.inertia = 0.9;
                    config.particles.kick = 3.0;
                },
                Colorize::Density,
                true,
            ),
            case(
                "notes coloring the sand",
                |config| config.mapping.transition_ms = 800,
                Colorize::Note,
                true,
            ),
            case(
                "amplitude colors on a turning drum",
                |config| config.plate.shape = Shape::Circle,
                Colorize::Amplitude,
                false,
            ),
            case(
                "amplitude colors on a square plate",
                |_| {},
                Colorize::Amplitude,
                true,
            ),
        ];
        for case in cases {
            let build = |full: bool| {
                let mut engine = engine_with(case.configure, |scheme| {
                    light_gradient(scheme);
                    scheme.colorize = case.colorize;
                });
                if full {
                    engine.disable_rest();
                }
                engine
            };
            let (mut resting, mut full) = (build(false), build(true));
            let mut rested = 0;
            for (index, audio) in script().iter().enumerate() {
                let drawn = drive(&mut resting, audio);
                drive(&mut full, audio);
                assert!(
                    resting.frame() == full.frame(),
                    "{}: frame {index} differs (drawn: {drawn})",
                    case.name
                );
                rested += usize::from(!drawn);
            }
            if case.rests {
                assert!(rested > 150, "{}: rested only {rested} times", case.name);
            } else {
                assert_eq!(rested, 0, "{} has moving colors and cannot rest", case.name);
            }
        }
    }

    #[test]
    fn a_notice_or_a_key_breaks_the_rest_and_a_resize_redraws() {
        let mut engine = warmed(|config| config.hud.enabled = true, |_| {});
        for _ in 0..200 {
            drive(&mut engine, &[0.0; 800]);
        }
        assert!(engine.is_resting());
        engine.notify(NoticeKind::Info, "hello");
        assert!(drive(&mut engine, &[0.0; 800]));
        assert!(row(&engine, 39).contains("hello"));
        // The notice stays for a few seconds, redrawn each step, then goes.
        let mut steps = 0;
        while engine.notice().is_some() {
            drive(&mut engine, &[0.0; 800]);
            steps += 1;
            assert!(steps < 1_000);
        }
        assert!(!row(&engine, 39).contains("hello"));
        for _ in 0..10 {
            drive(&mut engine, &[0.0; 800]);
        }
        assert!(engine.is_resting());
        engine.resize(60, 30).unwrap();
        assert!(drive(&mut engine, &[0.0; 800]));
        assert_eq!(engine.frame().width(), 60);
        engine.scatter();
        assert!(drive(&mut engine, &[0.0; 800]));
        // Too small for a plate: one message, then rest.
        engine.resize(3, 2).unwrap();
        assert!(drive(&mut engine, &[0.0; 800]));
        assert!(!drive(&mut engine, &[0.0; 800]));
        assert!(engine.is_resting());
        // A frozen engine rests too, until something changes.
        engine.resize(80, 40).unwrap();
        drive(&mut engine, &[0.0; 800]);
        engine.toggle_pause();
        assert!(drive(&mut engine, &[0.0; 800]));
        assert!(!drive(&mut engine, &[0.0; 800]));
        engine.toggle_pause();
        assert!(drive(&mut engine, &[0.0; 800]));
    }

    #[test]
    fn the_frame_rate_shown_ignores_the_long_step_after_a_rest() {
        let mut engine = engine(|config| {
            config.hud.enabled = true;
            config.hud.format = "{fps}".to_owned();
        });
        let mut synth = DemoSynth::new(42);
        for _ in 0..150 {
            drive(&mut engine, &frame_audio(Some(&mut synth)));
        }
        for _ in 0..300 {
            drive(&mut engine, &[0.0; 800]);
        }
        assert!(engine.is_resting());
        // The loop sleeps in 50 ms ticks while resting; the first frame after
        // sound returns is stepped by the time that passed.
        let mut audio = Vec::new();
        synth.pull(0.05, &mut audio).unwrap();
        engine.feed(&audio);
        assert!(engine.step(0.05, None).unwrap());
        assert!(row(&engine, 39).contains("60"), "{}", row(&engine, 39));
    }

    #[test]
    fn a_trail_gets_time_to_fade_before_the_plate_rests() {
        assert_eq!(settle_frames(0.0), 2);
        assert_eq!(settle_frames(-1.0), 2);
        assert_eq!(settle_frames(f32::NAN), 2);
        assert_eq!(settle_frames(0.5), 22);
        let fading = settle_frames(0.95);
        assert!((270..275).contains(&fading), "{fading}");
        assert_eq!(settle_frames(1.0), 3_600);
        // And the engine really draws that many silent frames first.
        let mut engine = warmed(|config| config.render.trail = 0.95, |_| {});
        let mut drawn = 0;
        for _ in 0..900 {
            drawn += usize::from(drive(&mut engine, &[0.0; 800]));
        }
        // The fade starts when the sound has died away, about a second in.
        let fading = fading as usize;
        assert!(drawn >= fading && drawn < fading + 120, "{drawn}");
    }

    fn sleeper(seconds: u32) -> Engine {
        let mut engine = engine(|config| config.general.sleep_timer = seconds);
        let mut synth = DemoSynth::new(42);
        for _ in 0..150 {
            drive(&mut engine, &frame_audio(Some(&mut synth)));
        }
        engine
    }

    fn is_blank(engine: &Engine) -> bool {
        engine
            .frame()
            .cells()
            .iter()
            .all(|cell| cell.character == ' ')
    }

    /// Feed silence until the plate sleeps; returns how many frames it took.
    fn silence_until_asleep(engine: &mut Engine) -> usize {
        for frames in 1..=2_000 {
            drive(engine, &[0.0; 800]);
            if engine.is_asleep() {
                return frames;
            }
        }
        panic!("the plate never fell asleep");
    }

    #[test]
    fn the_sleep_timer_blanks_the_plate_after_that_much_silence() {
        let mut engine = sleeper(2);
        assert!(!is_blank(&engine));
        let frames = silence_until_asleep(&mut engine);
        // The timer runs from the moment the sound has died away, which takes
        // about a second after the music stops.
        assert!((120..260).contains(&frames), "{frames} frames");
        drive(&mut engine, &[0.0; 800]);
        assert!(is_blank(&engine));
        // The sleeping plate is at rest: nothing is drawn any more.
        assert!(!drive(&mut engine, &[0.0; 800]));
        assert!(engine.is_resting());
        // Sound wakes it, with the sand where it was.
        let sand = engine.particles.x().len();
        assert!(drive(
            &mut engine,
            &frame_audio(Some(&mut DemoSynth::new(42)))
        ));
        assert!(!engine.is_asleep());
        assert!(!is_blank(&engine));
        assert_eq!(engine.particles.x().len(), sand);
    }

    #[test]
    fn a_sleeping_plate_hides_the_status_row_and_the_border_too() {
        let mut engine = engine(|config| {
            config.general.sleep_timer = 1;
            config.hud.enabled = true;
            config.border.style = BorderStyle::Double;
            config.border.title = "title".to_owned();
        });
        let mut synth = DemoSynth::new(42);
        for _ in 0..120 {
            drive(&mut engine, &frame_audio(Some(&mut synth)));
        }
        assert!(text(&engine).contains("title"));
        silence_until_asleep(&mut engine);
        drive(&mut engine, &[0.0; 800]);
        assert!(is_blank(&engine));
    }

    #[test]
    fn keys_and_notices_wake_a_sleeping_plate_which_then_falls_asleep_again() {
        let mut engine = sleeper(1);
        silence_until_asleep(&mut engine);
        drive(&mut engine, &[0.0; 800]);
        assert!(is_blank(&engine));
        engine.wake();
        assert!(drive(&mut engine, &[0.0; 800]));
        assert!(!is_blank(&engine));
        // Awake for the whole timer, then asleep again.
        for _ in 0..50 {
            drive(&mut engine, &[0.0; 800]);
        }
        assert!(!is_blank(&engine));
        for _ in 0..30 {
            drive(&mut engine, &[0.0; 800]);
        }
        assert!(is_blank(&engine));
        engine.notify(NoticeKind::Error, "audio input lost");
        assert!(drive(&mut engine, &[0.0; 800]));
        assert!(!is_blank(&engine));
        assert!(row(&engine, 39).contains("audio input lost"));
    }

    #[test]
    fn a_zero_sleep_timer_never_sleeps() {
        let mut engine = sleeper(0);
        for _ in 0..3_600 {
            drive(&mut engine, &[0.0; 800]);
        }
        assert!(!engine.is_asleep() && !is_blank(&engine));
        // And lowering it on a reload takes effect on the next step.
        let mut config = engine.config().clone();
        config.general.sleep_timer = 5;
        engine.reconfigure(config).unwrap();
        assert!(drive(&mut engine, &[0.0; 800]));
        assert!(is_blank(&engine), "an hour of silence is long past 5 s");
    }
}
