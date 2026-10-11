# grava — project spec & init prompt

> For Codex. Read the whole file before writing any code, then start with **Milestone 0** (section 14).

## 0. How we work

- Reply to me in **Vietnamese**. Write all code, comments, docs and commit messages in **English**.
- Work one milestone at a time. Finish each one with `cargo fmt`, `cargo clippy --all-targets -- -D warnings` (also with `--all-features` when the system libraries are installed) and `cargo test`, then give me a short summary: what changed, how to try it, what is left. Don't start the next milestone until the current one is green.
- Automatically create a Git commit after each completed, green milestone. Keep commit messages in English and include only that milestone's changes.
- You may not have an audio server or an interactive terminal. **Never launch the interactive UI in your own shell** (it takes over the terminal). Verify with unit tests, `--input demo` and headless mode (section 9.7). I test real audio capture myself on Fedora Workstation (GNOME, PipeWire + pipewire-pulse).
- cava (https://github.com/karlstav/cava, MIT) is the UX reference. Useful files: `README.md`, `example_files/config`, `cavacore.c`, `input/pipewire.c`, `input/pulse.c`, `output/terminal_noncurses.c`. Learn from it, don't translate it. If you port a piece of logic closely, credit it in a comment.
- Items marked **[decided]** are settled. If one turns out to be wrong or impractical, explain why and propose an alternative before deviating. Ask before adding dependencies not listed in section 4.
- In Milestone 0, create `AGENTS.md` containing this working agreement, the build/test commands and a pointer to `SPEC.md`, so later sessions pick it up automatically.

## 1. What we're building

`grava` is a terminal audio visualizer for Linux, written in Rust. Like cava, it captures whatever the computer is playing and animates it in the terminal. Instead of spectrum bars, it shows **sand on a vibrating plate or drum head**: grains are thrown off the parts of the surface that vibrate and collect on the nodal lines, where the surface stays still, so the sand draws a Chladni figure. Every note has its own figure; when the note changes, the sand flows into the new one.

Experience goals:

1. Beautiful at 60 fps: fine sand made of Braille dots, smooth flow between figures, no flicker.
2. Musically meaningful: same note → same figure; higher notes → more intricate figures; louder → more shaking; beats make the sand jump; silence → the sand stays put, like a real plate.
3. Runs on any Linux distro (Fedora, Arch, Debian/Ubuntu, Void glibc and musl, Alpine…) with no distro-specific code.
4. Riceable: colors of everything (sand, border, every piece of text), themes, glyphs, layout, HUD, with live reload.
5. cava ergonomics: one binary, XDG config, works with no config at all.

Non-goals for v1: GUI/SDL/OpenGL windows, macOS/Windows/BSD, audio playback, MIDI input.

## 2. Name

Crate and binary: **`grava`** (Spanish for "gravel"; a sister name to cava: **GR**anular **A**udio **V**isu**A**lizer). Config dir: `$XDG_CONFIG_HOME/grava/`, fallback `~/.config/grava/`. License: MIT.

## 3. Architecture [decided]

One Cargo package: a library (`src/lib.rs`) plus a thin binary (`src/main.rs`). Keep `dsp`, `chladni` and `sim` free of terminal and audio-backend code so the core can be reused later, the way cava split out `cavacore`.

Threads:

- **Capture thread** (owned by the audio backend): decode samples to f32, mix down to mono, push into a lock-free SPSC ring buffer (`rtrb`). Never block or allocate in the audio callback; on overflow, drop data.
- **Main thread**, one iteration per frame:
  1. Drain the ring buffer into the analysis window.
  2. Analyze → `AudioFeatures`.
  3. Map features → target mode weights.
  4. Crossfade the active field.
  5. Step the sand simulation.
  6. Rasterize → colorize → diff → one write to the terminal.
  7. Handle keys, resize, signals and config reload. Use `crossterm::event::poll(remaining_frame_time)` as the frame sleep so keys stay responsive.

Layout (rename freely, keep the boundaries):

```text
src/
  main.rs        CLI entry; prints errors only after the terminal is restored
  lib.rs
  app.rs         app state, main loop, key actions, idle/sleep
  cli.rs         clap definitions
  config/        schema.rs, load.rs (paths, layering, validation), color.rs,
                 theme.rs (built-in, file, pywal), watch.rs (mtime polling)
  audio/         mod.rs (trait, format, auto-detect), pulse.rs, pipewire.rs, alsa.rs,
                 fifo.rs, stdin.rs, demo.rs, pcm.rs (raw PCM decoding)
  dsp/           analyzer.rs, loudness.rs, peaks.rs, chroma.rs, onset.rs
  chladni/       square.rs, circle.rs, bessel.rs, modes.rs, field.rs, mapping.rs
  sim/           particles.rs
  render/        canvas.rs, glyphs.rs, colorize.rs, frame.rs, term.rs, hud.rs
assets/
  config.toml    the fully commented default config (embedded)
  themes/*.toml  built-in themes (embedded)
```

## 4. Dependencies

Core, always compiled, pure Rust (no system libraries):
`clap` (derive) + `clap_complete` + `clap_mangen`, `serde` + `toml` + `serde_json`, `crossterm`, `realfft`, `rtrb`, `fastrand` (seedable), `libm` (`jn` for drum modes), `signal-hook`, `anyhow` (binary), `thiserror` (library).

Audio backends as Cargo features [decided]:

| Feature | Crate | Build-time system deps | Purpose |
|---|---|---|---|
| `pulse` *(default)* | `pulseaudio` 0.3.x (pure-Rust client of the PulseAudio protocol) | none | PulseAudio, and PipeWire through pipewire-pulse; covers most desktops |
| `pipewire` | `pipewire` 0.9.x (pipewire-rs) | libpipewire-0.3 headers, libclang | native PipeWire |
| `alsa` | `alsa` | alsa-lib headers | ALSA loopback (`snd-aloop`) setups without a sound server |

The `fifo`, `stdin` and `demo` inputs are always available.

Notes:

- The default build must need nothing but a Rust toolchain, so `cargo install grava` works on any distro and fully static musl binaries are possible.
- If the `pulseaudio` crate can't reliably record from a monitor source, tell me. The fallback is `libpulse-simple-binding` (needs libpulse headers at build time).
- Don't use `cpal`: it requires ALSA headers even for its PipeWire/PulseAudio hosts, which breaks the zero-dependency default build.
- Don't use `pipewire-native`: its README says audio streaming is not implemented yet.

## 5. Audio input

### 5.1 Interface

Backends implement a small trait: **open** (connect and negotiate the format → `AudioFormat { sample_rate, channels }`) and **run** (blocking loop on the capture thread that writes mono f32 samples in −1..1 into the `rtrb::Producer` until a stop flag is set). Everything downstream must work at any sample rate.

Channel mixing: `channel_mix = "average" | "left" | "right"`.

### 5.2 Selection

`method = "auto"` tries, in order: `pipewire` (if compiled and a server answers) → `pulse` (if a socket exists: `$PULSE_SERVER`, else `$XDG_RUNTIME_DIR/pulse/native`) → `alsa` (if compiled). If nothing works, exit with a message that lists what was tried and what to do next: install/start pipewire-pulse, rebuild with `--features pipewire`, or pipe audio in with `--input stdin` / `fifo`.

### 5.3 Sources

`source = "auto"` means **the monitor of the default output** — what you hear, not the microphone.

- **pulse**: record from `@DEFAULT_MONITOR@`; if the server can't resolve it, query server info → default sink → its monitor source. Check whether recording a monitor through pipewire-pulse makes GNOME show its "recording" indicator; if it does, try passing `node.virtual = "true"` in the stream's property list and document the result.
- **pipewire**: a capture stream with `stream.capture.sink = true` targeting the default sink, as cava does. Mark the node virtual (`node.virtual = true`); cava does this so the desktop doesn't show a recording notification. Otherwise `source` is a node name or serial.
- **alsa**: default `hw:Loopback,1` (cava's example).
- **fifo**: a path (default `/tmp/mpd.fifo`) with raw PCM settings `sample_rate`, `sample_format` (`s16le|s32le|f32le`) and `channels`. Document the MPD `audio_output` snippet in the README.
- **stdin**: same raw PCM settings. README example: `parec -d @DEFAULT_MONITOR@ --format=s16le --rate=48000 --channels=2 | grava -i stdin`.
- **demo**: a deterministic built-in synth that needs no audio system. A melody that walks through all 12 pitch classes across several octaves (≈0.8 s per note, a few harmonics), a kick drum on every beat and short silences. Seedable.

`--list-sources` prints the sources of the selected backend (name and description; mark monitors and the default).

If the server disappears (for example PipeWire restarts), keep the UI alive, show "no audio" in the HUD and retry every 2 s.

## 6. Analysis (`dsp`)

Run every frame on the latest `fft_size` samples (default 4096, a power of two). Preallocate everything; no allocations per frame.

- **Spectrum**: Hann window → real FFT (`realfft`) → magnitudes, limited to `low_cutoff..high_cutoff` (default 40–8000 Hz).
- **Loudness**: RMS → attack/release smoothing (default 20 ms / 250 ms) → automatic gain like cava's autosens (raise gain slowly while it's quiet, cut it quickly when energy > 1) → × `sensitivity` → noise gate (below `noise_gate`, energy = 0). With `autosens = false`, use `sensitivity` only. Target: typical music ≈ 0.6, loud passages up to ~1.5.
- **Peaks**: local maxima with parabolic interpolation for sub-bin frequency; keep the strongest 32 above a relative threshold (5 % of the strongest). The chroma folds all of them; the strongest 8 are also reported in `AudioFeatures::peaks`, which only the `spectrum` mapping reads.
- **Chroma / pitch**: fold those interpolated peaks (not raw bins: below ~200 Hz an FFT bin is wider than a semitone, so bin centers can straddle a note and never name it) in 55–2000 Hz into 12 pitch classes (`midi = 69 + 12·log2(f/440)`, `pc = round(midi) mod 12`, 0 = C), weighted by magnitude², smoothed with an EMA (~150 ms). Output the dominant class and a confidence; take the octave from the strongest peak belonging to that class.
- **Onsets**: spectral flux on log magnitudes (sum of positive differences), adaptive threshold = mean + k·std over the last ~1 s, refractory period ~100 ms → `onset` strength 0..1.

```rust
pub struct AudioFeatures {
    pub energy: f32,             // 0 = silent, ~0.6 typical, up to ~1.5
    pub onset: f32,              // 0..1, non-zero only on onset frames
    pub chroma: [f32; 12],
    pub pitch_class: Option<u8>, // 0 = C … 11 = B
    pub octave: i8,
    pub confidence: f32,         // 0..1
    pub dominant_hz: Option<f32>,
    pub peaks: [(f32, f32); 8],  // (hz, magnitude)
    pub peak_count: usize,
}
```

## 7. Chladni model (`chladni`)

### 7.1 Square plate (`shape = "square"`)

Plate coordinates x, y ∈ [0, 1]:

    f(x, y) = X_n(x)·X_m(y) + s·X_m(x)·X_n(y)

- `edge = "free"` (default): X_k(t) = cos(kπt), n ≥ 0. `edge = "fixed"`: X_k(t) = sin(kπt), n ≥ 1.
- `symmetry = "anti"` (default): s = −1, requires n ≠ m. `"sym"`: s = +1. `"mixed"`: alternate along the catalog.

### 7.2 Circular drum head (`shape = "circle"`)

Polar coordinates r ∈ [0, 1], θ:

    u(r, θ) = J_n(j_{n,k}·r)·cos(n·(θ − θ₀)),   n ≥ 0, k ≥ 1

j_{n,k} is the k-th positive zero of J_n. Find the zeros at startup by scanning `libm::jn` for sign changes and refining with bisection; cache them. Nodal lines are n diameters plus (k − 1) circles. θ₀ drifts slowly (`rotate_speed`, rad/s) so degenerate figures don't look frozen.

### 7.3 Mode catalog

Enumerate modes with n, m ≤ 16 (square) or n ≤ 16, k ≤ 8 (circle) and sort them by eigenfrequency: square by n² + m², circle by j_{n,k} (ties: smaller n first). **The catalog order must not depend on the terminal size.** `max_order` (`"auto"` or an integer) only limits which entries are reachable; when the mapped entry is out of reach, `note` mapping falls back to the same pitch class in a lower tier and `frequency` mapping clamps. This way a note keeps its figure after a resize. Auto: keep the nodal-line spacing at ≥ ~6 sub-pixels for the current plate size (clamp 4..16). A drum mode `J(n,k)` has the equivalent order `ceil(2·j_{n,k}/π)`, which gives its nodal circles the spacing of a square mode of that order.

**Hollow drum modes are left out of the circle catalog.** For large `n` the Bessel function `J_n` is negligible below its first turning point, so a "whispering gallery" mode such as `J(12,1)` leaves a core that barely moves: sand scattered there never settles and the figure is noise instead of nodal lines. A mode is hollow when its dead radius — the radius up to which `|J_n(j_{n,k}·r)|` stays below 5 % of its maximum — exceeds 0.4 of the drum radius (`HOLLOW_RADIUS` in `chladni/circle.rs`; this removes `J(8..16,1)`, `J(12..16,2)` and `J(16,3)`). The filter is a property of the mode alone, so the catalog order still does not depend on the terminal size; the remaining modes are sorted by `j_{n,k}` as before (121 of the 136).

### 7.4 Fields

Rasterize each mode once into a G×G grid (G = 256, or the plate size in sub-pixels if larger) of |f| normalized so that max = 1. Cache lazily (~64 grids, LRU). Sample bilinearly; compute the gradient by central differences. The active field combines up to K modes with weights wᵢ as incoherent vibration:

    A = sqrt(Σ wᵢ²·fᵢ²) / sqrt(Σ wᵢ²)        (A ∈ [0, 1])

Rebuild the combined grid only when the weights change.

### 7.5 Mapping (`mapping.mode`)

- **`note`** (default): the figure depends on the pitch class. With `octave_complexity = true`: tier = clamp(octave − `base_octave`, 0, tiers − 1) and mode = catalog[tier·12 + pitch_class]. Users can pin a figure to a note with `mapping.notes`.
- **`frequency`**: the dominant frequency on a log scale between the cutoffs → catalog index.
- **`spectrum`**: the top `peaks` peaks, each mapped like `frequency`, weights ∝ magnitude^1.5.

Stability rules for all modes: a candidate replaces the target only after winning for `hold_ms`; ignore pitch below `min_confidence`; crossfade weights over `transition_ms`; in silence keep the current field.

## 8. Sand simulation (`sim`)

Struct-of-arrays storage (`x`, `y`, optional `vx`, `vy`). `count = "auto"` → `density` × plate area in sub-pixels, clamped to 2 000–150 000.

Per-particle update. This is a starting point; tune the constants by looking at headless snapshots:

```text
a   = A(p)                                         local vibration, 0..1
e   = energy · jitter
k   = dt · 60                                      frame-rate normalization
p  += gaussian2() · step · e · a · sqrt(k)         shaking (Brownian motion scales with √dt)
p  -= unit(∇A(p)) · settle · step · e · a · k      gentle drift towards nodal lines
p  += gaussian2() · kick · onset · step · (0.3 + a)   beat: everything jumps a little
```

`step` is configured in sub-pixels and converted to plate units. Optional `inertia` (0 = pure random walk) carries over part of the previous displacement. Boundaries: reflect at the square's edges / back inside the circle's rim. The `s` key re-scatters all sand uniformly.

Budget: updating and rasterizing 50k particles takes < 2 ms per frame in release mode on a laptop CPU, single-threaded, with zero allocations.

## 9. Rendering (`render`)

### 9.1 Layout

The plate is a square (or the circle inscribed in it) in physical units, centered inside `padding`. Use `cell_aspect` (cell width ÷ height, default 0.5) and the glyph set's sub-pixel grid so the plate is never stretched. `fit = "stretch"` fills the available area instead.

### 9.2 Glyph sets (`render.glyphs`)

- `braille`: 2×4 sub-pixels, `U+2800 + bits`. Bit for (column, row): (0,0)=0x01, (0,1)=0x02, (0,2)=0x04, (1,0)=0x08, (1,1)=0x10, (1,2)=0x20, (0,3)=0x40, (1,3)=0x80.
- `quadrant`: 2×2. Index TL=1, TR=2, BL=4, BR=8 into `" ▘▝▀▖▌▞▛▗▚▐▜▄▙▟█"`.
- `half`: 1×2, `" ▀▄█"`.
- `ascii`: 1×1, density ramp `ascii_ramp` (default `" .:-=+*#%@"`).
- `auto` (default): `braille` when the locale is UTF-8 and `TERM` ≠ `linux`, otherwise `ascii`. The Linux console font has no Braille, and cava's README warns that output breaks without a UTF-8 locale.

### 9.3 Canvas

Count particles per sub-pixel (u16). The optional `trail` (0..1) keeps a fading afterimage: `buf = buf·trail + counts`. A dot is lit when its value ≥ `dot_threshold`. Cell density = sum over its sub-pixels ÷ (expected average × saturation), clamped to 0..1.

### 9.4 Color

- Values: `"default"` (the terminal's own fg/bg), ANSI names (`black` … `white`, `bright-black` … `bright-white`), palette index `"0"`–`"255"`, hex `"#rrggbb"`.
- ANSI names and indices follow the terminal palette, so pywal/wallust/matugen themes apply automatically. The built-in `default` theme uses only these.
- `color_mode = auto|truecolor|256|16|none`. Auto: truecolor if `COLORTERM` is `truecolor` or `24bit`, 16 colors on `TERM=linux`, otherwise 256 (quantize hex colors). Honor `NO_COLOR`.
- `colorize`: `solid` (the `sand` color), `density` (default), `radial`, `vertical`, `horizontal`, `amplitude` (local A: grains that are still shaking glow), `note` (12 `note_colors`, crossfaded when the note changes).
- Gradients: interpolate in OKLab, quantize to `gradient_steps` (default 24) so neighboring cells share colors and fewer escape codes are sent.
- Never paint the background unless `background` is set. Transparent terminals must stay transparent.

### 9.5 Terminal backend [decided: crossterm with a custom renderer; no ratatui, no ncurses]

Alternate screen, hidden cursor, raw mode. Keep the previous frame (cell = char + fg + bg) and write only the cells that changed; track the SGR state to avoid redundant escapes, move the cursor only when needed, and do one `write` per frame wrapped in synchronized output (`BeginSynchronizedUpdate` / `EndSynchronizedUpdate`, config `synchronized_output`). Restore the terminal on every exit path: quit, error, panic (panic hook), SIGINT/SIGTERM/SIGHUP. Survive any resize; if the terminal is too small, show a short message instead.

### 9.6 HUD

- Border around the plate: `none|rounded|square|double|thick`, plus `circle` (drawn with the plate's own glyphs) for drum mode, with an optional title on the border.
- Status line from `hud.format` with placeholders `{note} {octave} {hz} {mode} {shape} {theme} {fps} {particles} {source} {energy}`. `{mode}` renders as `(n,m)` or `J(n,k)`; `{energy}` as a small `▁▂▃▄▅▆▇█` meter. The border title accepts the same placeholders.
- Help overlay on `?`.
- Errors (a broken config on reload, lost audio) appear in the HUD for a few seconds instead of killing the app.
- Every piece of text has its own color key (section 10.3).

### 9.7 Headless mode

`grava --headless --frames N [--size 80x40] [--seed S] [--no-color] [-i demo]` runs the full pipeline without a TTY and prints the last frame to stdout (plain text, or with ANSI colors). In headless mode nothing reads the wall clock: dt = 1/fps and the demo source is pulled synchronously, so the output is identical for a given seed. `--bench` prints per-stage timings.

### 9.8 Idle and sleep

- **Resting.** A step that cannot change the picture (no sound, the figure has arrived, no fade, notice or trail is running, nothing is pending) skips the simulation, the rasterizing, the composing and the drawing. The loop then waits a 50 ms tick instead of a frame, and still drains the audio source and listens to the keyboard, so sound is noticed within one tick and a key at once. The sand does not move at zero energy, so resting is exact: a test compares every frame with the one the full pipeline draws.
- **Sleep.** `sleep_timer` seconds after the sound has died away (the noise gate closes about a second after the music stops) the plate goes blank: sand, border and status row. Sound, a key, or a notice (a reload, a lost input) wakes it and restarts the timer. A sleeping plate rests, so it costs no more than a resting one. `0` never sleeps.
- **Linux console** (`TERM=linux`): `auto` picks ASCII glyphs and 16 colors, borders and the energy meter are ASCII, and the screen is cleared instead of switching to an alternate screen, which the console lacks.

## 10. Configuration & theming

### 10.1 Files and layering

- Config: `-c PATH`, else `$XDG_CONFIG_HOME/grava/config.toml`, else `~/.config/grava/config.toml`. Themes: `<config dir>/themes/<name>.toml`.
- Precedence: built-in defaults < theme < `[colors]` overrides in the config < CLI flags.
- `assets/config.toml` is the fully commented default (every key documented, like cava's `example_files/config`), embedded with `include_str!`. A test checks that it parses to exactly `Config::default()`. `--print-default-config` prints it; `--init [--force]` writes it plus one sample theme. Nothing is created implicitly.
- Unknown keys → warning. Invalid values → an error naming the key and the allowed values.
- Live reload: when `live_reload = true`, poll the mtime of the config and of the active theme file every 500 ms, following symlinks (stow, chezmoi and home-manager use them). `r` or SIGUSR1 reloads everything; `c` or SIGUSR2 reloads colors only (same as cava). A broken file on reload keeps the previous config and shows the error in the HUD.

### 10.2 Default config (schema)

```toml
[general]
framerate = 60
autosens = true
sensitivity = 1.0
sleep_timer = 0          # seconds of silence before sleeping (0 = never; it still idles when the sand is still)
live_reload = true

[input]
method = "auto"          # auto | pulse | pipewire | alsa | fifo | stdin | demo
source = "auto"          # auto = monitor of the default output
channel_mix = "average"  # average | left | right
sample_rate = 48000      # fifo/stdin only
sample_format = "s16le"  # fifo/stdin only: s16le | s32le | f32le
channels = 2             # fifo/stdin only

[analysis]
fft_size = 4096
low_cutoff = 40
high_cutoff = 8000
noise_gate = 0.02
attack_ms = 20
release_ms = 250

[plate]
shape = "square"         # square | circle
edge = "free"            # square only: free | fixed
symmetry = "anti"        # square only: anti | sym | mixed
max_order = "auto"       # or an integer 4..16
rotate_speed = 0.05      # circle only, rad/s

[mapping]
mode = "note"            # note | frequency | spectrum
octave_complexity = true
base_octave = 2
min_confidence = 0.25
hold_ms = 120
transition_ms = 350
peaks = 4                # spectrum mode only
# Pin figures to notes: [n, m] for the square plate, [n, k] for the drum
# notes = { "C" = [0, 3], "C#" = [1, 4], "A" = [2, 5] }

[particles]
count = "auto"           # or an integer
density = 0.45           # particles per sub-pixel when count = "auto"
step = 1.2               # sub-pixels per frame at full shaking
jitter = 1.0
settle = 0.4
inertia = 0.0
kick = 1.0

[render]
glyphs = "auto"          # auto | braille | quadrant | half | ascii
ascii_ramp = " .:-=+*#%@"
dot_threshold = 1
trail = 0.0              # 0..1 afterimage
color_mode = "auto"      # auto | truecolor | 256 | 16 | none
gradient_steps = 24
synchronized_output = true
cell_aspect = 0.5

[layout]
fit = "square"           # square | stretch
padding = [1, 2]         # [vertical, horizontal] in cells
align = "center"         # center | top | bottom | left | right

[border]
style = "none"           # none | rounded | square | double | thick | circle
title = ""               # e.g. " {note}{octave} "
title_align = "center"   # left | center | right

[hud]
enabled = false
position = "bottom"      # top | bottom
format = "{note}{octave}  {mode}  {fps} fps"

[colors]
theme = "default"        # a built-in name, a file in themes/, or "pywal"
# Any key below overrides the theme:
# background = "default"
# sand = "default"
# colorize = "density"   # solid | density | radial | vertical | horizontal | amplitude | note
# gradient = ["8", "4", "6", "15"]
# note_colors = [ ...12 colors, C to B... ]
# border = "8"
# title = "bright-white"
# hud_text = "7"
# hud_accent = "6"
# hud_dim = "8"
# hud_error = "1"
```

### 10.3 Themes

A theme file can set any of: `background`, `sand`, `colorize`, `gradient`, `note_colors`, `border`, `title`, `hud_text`, `hud_accent`, `hud_dim`, `hud_error`.

```toml
# ~/.config/grava/themes/dune.toml
background = "default"
sand = "#e8d5b0"
colorize = "density"
gradient = ["#4a3b2a", "#a68a64", "#e8d5b0", "#fff6e0"]
border = "#6b5a45"
title = "#f2c879"
hud_text = "#cbb894"
hud_accent = "#f2c879"
hud_dim = "#6b5a45"
hud_error = "#e06c5a"
```

Built-in themes (embedded; a user file with the same name wins): `default` (terminal palette only), `sand`, `mono`, `catppuccin-mocha`, `gruvbox-dark`, `nord`, `tokyo-night`, `rose-pine`. Use the official palette values. `theme = "pywal"` reads `~/.cache/wal/colors.json` (path configurable) and maps it sensibly, for example: sand = foreground, gradient = color1 → color4 → color6 → foreground, border and hud_dim = color8, title and hud_accent = color4, hud_error = color1. Document the mapping. `t` / `T` cycles themes at runtime; `--list-themes` lists them.

## 11. CLI & keybindings

```text
grava [OPTIONS]
  -c, --config <PATH>
  -i, --input <auto|pulse|pipewire|alsa|fifo|stdin|demo>
  -s, --source <SOURCE>
  -t, --theme <NAME>
      --shape <square|circle>
      --glyphs <auto|braille|quadrant|half|ascii>
      --particles <N|auto>
      --fps <N>
      --list-sources
      --list-themes
      --print-default-config
      --check-config
      --init [--force]
      --headless --frames <N> [--size <WxH>] [--seed <N>] [--no-color]
      --bench
      --completions <bash|zsh|fish>
      --man
  -V, --version          also lists the compiled audio features
```

| Key | Action |
|---|---|
| `q`, `Esc`, `Ctrl-C` | quit |
| `r` / `c` | reload config / reload colors only |
| `↑` / `↓` | sensitivity up / down |
| `←` / `→` | simpler / more intricate figures (shift tier) |
| `t` / `T` | next / previous theme |
| `p` | square ↔ circle |
| `m` | cycle mapping mode |
| `g` | cycle glyph set |
| `s` | scatter the sand |
| `Space` | freeze |
| `i` | toggle HUD |
| `?` | help |

## 12. Portability & packaging

- Linux only, but no distro-specific code: it must build and run on both glibc and musl.
- No runtime dependency on external tools (`pactl`, `pw-cli`, `wpctl`), systemd, D-Bus or any particular desktop.
- Respect `XDG_CONFIG_HOME`, `NO_COLOR`, `COLORTERM`, `TERM` and `LANG`/`LC_ALL`/`LC_CTYPE`.
- Rust edition 2024; declare `rust-version` as low as the dependencies allow.
- CI (GitHub Actions): fmt, clippy and test with default features; a job that installs the system libraries and builds with `--all-features`; a release job that produces static `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` binaries with default features.
- `packaging/`: AUR `PKGBUILD` (plus `grava-git`), Void `template` (xbps-src) and a Fedora `.spec` for COPR. Templates only; I'll submit them myself.
- README: a GIF placeholder; install (cargo, binaries, distros); build deps for the optional features (Fedora `pipewire-devel alsa-lib-devel clang-devel`; Debian/Ubuntu `libpipewire-0.3-dev libasound2-dev libclang-dev pkg-config`; Arch `libpipewire alsa-lib clang`; Void `pipewire-devel alsa-lib-devel clang`; verify the package names); capture troubleshooting (monitor vs. microphone, ALSA loopback, MPD fifo, piping with `parec`); a ricing guide; keybindings.

## 13. Code quality & tests

- `#![forbid(unsafe_code)]` everywhere except backend modules that truly need it; comment every `unsafe` block.
- No `unwrap`/`expect` outside tests unless a comment explains the invariant.
- Hot paths (capture callback, analysis, simulation, rendering) are allocation-free.
- Tests, at minimum:
  - color parsing;
  - the default config round-trips to `Config::default()`;
  - Braille and quadrant tables;
  - the anti-symmetric square mode is exactly 0 on the diagonal x = y;
  - Bessel zeros j₀,₁ ≈ 2.404826, j₀,₂ ≈ 5.520078, j₁,₁ ≈ 3.831706, j₂,₁ ≈ 5.135622 (±1e-5);
  - a 440 Hz sine → pitch class 9 (A), octave 4;
  - onsets fire on synthetic clicks but not on a steady tone;
  - mapping hysteresis: a brief blip doesn't switch the figure;
  - convergence: with a fixed mode and constant energy 0.8, after 300 steps at 60 fps the mean A over all particles is < 35 % of the starting mean;
  - headless output is identical for the same seed.

## 14. Milestones

**M0 — Scaffold.** Project, module skeleton, clap CLI, config schema and embedded default config, `--print-default-config`, `--check-config`, CI, `AGENTS.md`.
*Done when:* `cargo test` is green and the printed default config is valid TOML.

**M1 — Sand on a square plate.** Square modes, field cache, particles, canvas with Braille glyphs, terminal backend with diffing, headless mode, clean exit and resize. Until the DSP exists, `-i demo` may drive the figures directly (one figure every ~2 s at constant energy).
*Done when:* headless snapshots show crisp nodal lines, the convergence test passes, and I can watch sand flow into figures with `grava -i demo`.

**M2 — Listening.** DSP (loudness/AGC/gate, peaks, chroma, onsets), the three mapping modes, transitions, the real demo synth going through the analyzer, `stdin` and `fifo` inputs, circular drum modes.
*Done when:* the DSP tests are green, `parec … | grava -i stdin` follows my music, and the same note gives the same figure.

**M3 — Real capture.** `pulse` (default), `pipewire` and `alsa` backends, auto-detection, `--list-sources`, reconnect, helpful errors.
*Done when:* `--no-default-features`, default and `--all-features` all build, and plain `grava` works out of the box on my Fedora.

**M4 — Ricing.** Themes (built-in, files, pywal), color modes, all colorize modes, gradients, the other glyph sets, border/title/HUD, live reload and signals, all keybindings, help overlay.
*Done when:* every key in the default config has a visible effect and is documented.

**M5 — Polish & release.** Idle and sleep (≈0 % CPU in silence: skip simulation and drawing when nothing moves; `sleep_timer`), TTY fallback, `NO_COLOR`, completions and man page, README, packaging templates, release CI.
*Done when:* idle CPU is ≈ 0 and a new user can install and rice grava from the README alone.

## 15. Stretch goals (ask before starting)

- `--output json`: a stream of features (note, figure, energy) for waybar/eww widgets.
- Stereo mode: two plates side by side for the left and right channels.
- JACK backend.
- Sextant glyphs (Unicode 13) for a 2×3 sub-pixel grid.
- Kitty graphics protocol output for true pixels.
- Custom mode lists per theme.
