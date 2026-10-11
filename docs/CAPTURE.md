# Audio capture notes

How grava gets audio, and what to try when it does not. This is the source for
the README's capture troubleshooting section.

## What `grava` records by default

`input.method = "auto"` and `input.source = "auto"` record **the monitor of the
default output**: what you hear, not the microphone.

Auto tries the compiled backends in order and uses the first that connects:

1. `pipewire` (optional feature) if a PipeWire server answers,
2. `pulse` (default feature) if a PulseAudio socket exists: `$PULSE_SERVER`,
   then `$PULSE_RUNTIME_PATH/native`, then `$XDG_RUNTIME_DIR/pulse/native`,
3. `alsa` (optional feature).

If none works the error lists what was tried and why. `grava --list-sources`
prints the sources of the backend auto would use (or of `-i BACKEND`); `*`
marks what `source = "auto"` captures and `monitor` rows carry playback.

| Backend | Build needs | `source` takes |
|---|---|---|
| `pulse` | nothing (pure Rust) | a source name or index; works with PulseAudio and with PipeWire via `pipewire-pulse` |
| `pipewire` | `libpipewire-0.3` headers, libclang | a node name or serial; naming an output records its monitor |
| `alsa` | `alsa-lib` headers | a device such as `hw:Loopback,1` (the default) |
| `fifo` | nothing | a path, default `/tmp/mpd.fifo` |
| `stdin` | nothing | ignored |

With `source = "auto"`, `pulse` and `pipewire` follow the default output when it
changes (headphones plugged in, Bluetooth connecting). A named source stays
where it is.

## Server restarts

If the audio server disappears or the record stream is removed, grava stays
open, treats the input as silent, and tries to reconnect every two seconds.
Only a failure at startup ends the program, with a message before the terminal
is touched. The connection state is available to the HUD as
`SampleSource::connected()`.

## The "recording" indicator on GNOME

Every stream grava opens carries `node.virtual = true`: through `pulse` it is
passed in the stream's property list, through `pipewire` it is a stream
property, as cava does.

Verified on PipeWire 1.6.9, both through pipewire-pulse (`pulse`) and natively
(`pipewire`): the property reaches the PipeWire node (`pw-dump` shows
`"node.virtual": true`, `media.class = Stream/Input/Audio`,
`node.latency = 480/48000`), and with `source = "auto"` the native stream is
linked to the default output's monitor ports. Whether the GNOME Shell
status area shows its recording indicator for such a stream could not be
observed without the desktop session's display, so it is still to be confirmed
by running `grava` and looking at the top bar. If the indicator appears anyway,
the property is not enough on that GNOME version and the stream should be
treated as a desktop-specific problem rather than hidden with more flags.

## Troubleshooting

* `no PulseAudio server socket found`: start `pipewire-pulse` (Fedora and most
  desktops start it with the user session) or `pulseaudio`. Remote servers in
  `PULSE_SERVER` (`tcp:...`) are not supported.
* Silent plate while music plays: `grava --list-sources` and check that the
  `*` source is a `monitor` and that the music goes to that output. Pass
  `-s NAME` to record a different one.
* The sand reacts to the microphone: the default source was a microphone, pick
  the output's `.monitor` with `-s`.
* **ALSA loopback** (no sound server): `sudo modprobe snd-aloop`, play to
  `hw:Loopback,0` and let grava read `hw:Loopback,1`
  (`grava -i alsa`). The rate must be the player's: set `input.sample_rate`.
* **MPD**: add to `mpd.conf`, then `grava -i fifo`
  (set `sample_rate = 44100`, `channels = 2`, `sample_format = "s16le"` to match):

  ```text
  audio_output {
      type   "fifo"
      name   "grava"
      path   "/tmp/mpd.fifo"
      format "44100:16:2"
  }
  ```
* **Any other source**, through a pipe:

  ```sh
  parec -d @DEFAULT_MONITOR@ --format=s16le --rate=48000 --channels=2 | grava -i stdin
  ```

## What has been verified where

| Backend | Verified |
|---|---|
| `pulse` | Real PipeWire 1.6.9 via pipewire-pulse: 440 Hz read as A4 (439.9 Hz), node properties, listing; fake-server tests for reconnect and default-follow |
| `pipewire` | Same server, natively: A4 at 439.9 Hz, listing with the default marked, `auto` linked to the default monitor, node properties |
| `alsa` | Opens, negotiates a format and delivers samples at the exact rate; the content (pitch) has **not** been checked, because that needs an `snd-aloop` loopback, which needs root |

## Testing without disturbing anyone

`cargo test` never touches your audio server: the PulseAudio client is tested
against an in-process fake server. To check the real thing, record a null sink
that plays a tone nobody hears:

```sh
pactl load-module module-null-sink sink_name=grava_test
paplay -d grava_test tone.wav &   # a 440 Hz stereo 48 kHz file
GRAVA_LIVE_SOURCE=grava_test.monitor cargo test --lib audio::pulse::tests::live_ -- --ignored --nocapture
GRAVA_LIVE_SOURCE=grava_test cargo test --all-features --lib audio::pipewire::tests::live_ -- --ignored --nocapture
pactl unload-module <module id printed by the first command>
```

For `alsa`, load `snd-aloop`, play the tone to `hw:Loopback,0` and run
`GRAVA_LIVE_DEVICE=hw:Loopback,1 cargo test --all-features --lib audio::alsa::tests::live_ -- --ignored --nocapture`.
Do not substitute a device that can fall back to the default input: that is the
microphone.
