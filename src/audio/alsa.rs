//! ALSA capture, for setups without a sound server: typically a loopback
//! device (`modprobe snd-aloop`) that a player writes to as `hw:Loopback,0`
//! and grava reads as `hw:Loopback,1`, which is the default `source`.
//!
//! The sample rate is `input.sample_rate`, because a loopback must be read in
//! the format the player chose. Float and 16-bit samples are understood.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use alsa::{
    Direction, ValueOr,
    device_name::HintIter,
    pcm::{Access, Format, HwParams, IoFormat, PCM, State},
};
use rtrb::Producer;

use crate::config::schema::{ChannelMix, InputConfig};

use super::{
    AudioError, AudioFormat, Capture, Link, RETRY_INTERVAL, pcm::mix_interleaved,
    select::SourceEntry, supervise,
};

/// The loopback's capture side, as in cava's example configuration.
pub const DEFAULT_DEVICE: &str = "hw:Loopback,1";
const CHANNELS: u32 = 2;
/// How long to wait for audio before checking the stop flag.
const WAIT: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
enum AlsaError {
    #[error("cannot open ALSA device `{device}`: {source}")]
    Open {
        device: String,
        #[source]
        source: alsa::Error,
    },
    #[error("device `{device}` cannot capture 16-bit or float audio: {source}")]
    NoFormat {
        device: String,
        #[source]
        source: alsa::Error,
    },
    #[error("ALSA failure on `{device}`: {source}")]
    Failed {
        device: String,
        #[source]
        source: alsa::Error,
    },
    #[error("`{0}` is not a valid ALSA device name")]
    BadName(String),
}

impl From<AlsaError> for AudioError {
    fn from(error: AlsaError) -> Self {
        AudioError::Unsupported(error.to_string())
    }
}

/// The device named by `source`; `auto` means the loopback's capture side.
fn device_name(source: &str) -> Result<String, AlsaError> {
    let device = if source == "auto" {
        DEFAULT_DEVICE
    } else {
        source
    };
    // The binding turns names into C strings and cannot take a NUL.
    if device.contains('\0') {
        return Err(AlsaError::BadName(device.replace('\0', "\\0")));
    }
    Ok(device.to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Samples {
    Signed16,
    Float,
}

/// An opened and configured capture PCM.
struct Session {
    pcm: PCM,
    device: String,
    samples: Samples,
    channels: usize,
    rate: u32,
}

impl Session {
    fn open(device: &str, rate: u32) -> Result<Self, AlsaError> {
        let open_error = |source| AlsaError::Open {
            device: device.to_owned(),
            source,
        };
        let pcm = PCM::new(device, Direction::Capture, false).map_err(open_error)?;
        let (samples, channels, rate) = {
            let hardware = HwParams::any(&pcm).map_err(open_error)?;
            hardware
                .set_access(Access::RWInterleaved)
                .map_err(open_error)?;
            let samples = if hardware.set_format(Format::float()).is_ok() {
                Samples::Float
            } else {
                hardware
                    .set_format(Format::s16())
                    .map_err(|source| AlsaError::NoFormat {
                        device: device.to_owned(),
                        source,
                    })?;
                Samples::Signed16
            };
            let channels = hardware.set_channels_near(CHANNELS).map_err(open_error)?;
            let rate = hardware
                .set_rate_near(rate, ValueOr::Nearest)
                .map_err(open_error)?;
            // About ten milliseconds per period keeps the sand responsive.
            hardware
                .set_period_size_near(i64::from(rate / 100), ValueOr::Nearest)
                .map_err(open_error)?;
            pcm.hw_params(&hardware).map_err(open_error)?;
            (samples, channels as usize, rate)
        };
        pcm.prepare().map_err(open_error)?;
        Ok(Self {
            pcm,
            device: device.to_owned(),
            samples,
            channels: channels.max(1),
            rate,
        })
    }

    /// Read until stopped. Overruns are recovered from and not reported.
    fn record(
        &mut self,
        mix: ChannelMix,
        producer: &mut Producer<f32>,
        stop: &AtomicBool,
    ) -> Result<(), AlsaError> {
        match self.samples {
            Samples::Float => self.capture::<f32>(mix, producer, stop, |sample| sample),
            Samples::Signed16 => {
                self.capture::<i16>(mix, producer, stop, |sample| f32::from(sample) / 32_768.0)
            }
        }
    }

    fn capture<S: IoFormat + Copy + Default>(
        &mut self,
        mix: ChannelMix,
        producer: &mut Producer<f32>,
        stop: &AtomicBool,
        convert: impl Fn(S) -> f32,
    ) -> Result<(), AlsaError> {
        let failed = |source| AlsaError::Failed {
            device: self.device.clone(),
            source,
        };
        let io = self.pcm.io_checked::<S>().map_err(failed)?;
        let frames = (self.rate / 100).max(64) as usize;
        let mut raw = vec![S::default(); frames * self.channels];
        let mut floats = Vec::with_capacity(raw.len());
        let mut mono = Vec::with_capacity(frames);
        while !stop.load(Ordering::Relaxed) {
            // A capture PCM that was just prepared (or recovered from an
            // overrun) delivers nothing, and never wakes `wait`, until started.
            if self.pcm.state() == State::Prepared {
                self.pcm.start().map_err(failed)?;
            }
            match self.pcm.wait(Some(WAIT.as_millis() as u32)) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    self.pcm.try_recover(error, true).map_err(failed)?;
                    continue;
                }
            }
            let count = match io.readi(&mut raw) {
                Ok(count) => count,
                Err(error) => {
                    // An overrun (EPIPE) or a suspend is not fatal.
                    self.pcm.try_recover(error, true).map_err(failed)?;
                    continue;
                }
            };
            floats.clear();
            floats.extend(
                raw[..count * self.channels]
                    .iter()
                    .map(|&sample| convert(sample)),
            );
            mono.clear();
            mix_interleaved(&floats, self.channels, mix, |sample| mono.push(sample));
            // A full ring drops samples; the device is never left waiting.
            let _ = producer.push_partial_slice(&mono);
        }
        Ok(())
    }
}

pub struct AlsaCapture {
    device: String,
    mix: ChannelMix,
    rate: u32,
    session: Option<Session>,
    link: Link,
    retry: Duration,
}

impl AlsaCapture {
    /// Open and configure the device. Problems are reported here, before the
    /// terminal is taken over.
    pub fn open(config: &InputConfig) -> Result<Self, AudioError> {
        let device = device_name(&config.source)?;
        let session = Session::open(&device, config.sample_rate)?;
        Ok(Self {
            device,
            mix: config.channel_mix,
            rate: session.rate,
            session: Some(session),
            link: Link::new(true),
            retry: RETRY_INTERVAL,
        })
    }
}

impl Capture for AlsaCapture {
    fn format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: self.rate,
            channels: CHANNELS as u16,
        }
    }

    fn label(&self) -> String {
        format!("alsa: {}", self.device)
    }

    fn link(&self) -> Option<Link> {
        Some(self.link.clone())
    }

    /// Record until stopped. If the device disappears (a USB interface is
    /// unplugged), try to reopen it every two seconds.
    fn run(&mut self, producer: &mut Producer<f32>, stop: &AtomicBool) -> Result<(), AudioError> {
        let (device, mix, rate) = (self.device.clone(), self.mix, self.rate);
        supervise(
            stop,
            &self.link,
            self.retry,
            self.session.take(),
            // The reopened device must keep the rate the analyzer was built for.
            || Session::open(&device, rate).map_err(AudioError::from),
            |session| {
                session
                    .record(mix, producer, stop)
                    .map_err(AudioError::from)
            },
        )
    }
}

/// The capture devices ALSA knows, for `--list-sources`. ALSA has no monitors;
/// loopback devices stand in for them.
pub fn list_sources() -> Result<Vec<SourceEntry>, AudioError> {
    let hints = HintIter::new_str(None, "pcm")
        .map_err(|source| AudioError::Unsupported(format!("cannot list ALSA devices: {source}")))?;
    Ok(hints
        .filter(|hint| hint.direction != Some(Direction::Playback))
        .filter_map(|hint| {
            let name = hint.name?;
            let loopback = name.contains("Loopback");
            Some(SourceEntry {
                description: hint
                    .desc
                    .map(|text| text.replace('\n', " - "))
                    .unwrap_or_default(),
                // The default is the loopback's capture side, `hw:Loopback,1`.
                default: loopback && (name.contains("DEV=1") || name.ends_with(",1")),
                monitor: loopback,
                name,
            })
        })
        .collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn auto_means_the_loopback_capture_side_and_names_pass_through() {
        assert_eq!(device_name("auto").unwrap(), "hw:Loopback,1");
        assert_eq!(
            device_name("hw:CARD=PCH,DEV=0").unwrap(),
            "hw:CARD=PCH,DEV=0"
        );
        assert_eq!(device_name("default").unwrap(), "default");
    }

    #[test]
    fn names_the_binding_would_panic_on_are_rejected_first() {
        let error = device_name("hw:\0evil").unwrap_err().to_string();
        assert!(error.contains("not a valid ALSA device name"), "{error}");
    }

    #[test]
    fn a_device_that_does_not_exist_names_itself_in_the_error() {
        let error = Session::open("hw:NoSuchCardAnywhere,9", 48_000)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("hw:NoSuchCardAnywhere,9"), "{error}");
        assert!(error.contains("cannot open"), "{error}");
    }

    #[test]
    fn listing_does_not_fail_without_sound_cards() {
        // Whatever ALSA reports on the build machine, the call itself works.
        let sources = list_sources().unwrap();
        assert!(sources.iter().all(|source| !source.name.is_empty()));
    }

    /// Needs a capture device that carries a 440 Hz tone, named by
    /// `GRAVA_LIVE_DEVICE`: the loopback's `hw:Loopback,1` after
    /// `sudo modprobe snd-aloop` and a player writing to `hw:Loopback,0`.
    /// Use nothing that can fall back to the default input: that is the
    /// microphone (the ALSA PipeWire plugin does exactly that when it cannot
    /// find its `NODE`, so it is not a stand-in for a loopback).
    #[test]
    #[ignore = "needs an ALSA device carrying a tone; see the comment above"]
    fn live_capture_hears_the_tone_as_a4() {
        use crate::{
            audio::{SampleSource, StreamSource},
            config::{Config, schema::InputMethod},
            dsp::Analyzer,
        };

        let Ok(device) = std::env::var("GRAVA_LIVE_DEVICE") else {
            return;
        };
        let config = InputConfig {
            method: InputMethod::Alsa,
            source: device,
            ..InputConfig::default()
        };
        let capture = AlsaCapture::open(&config).unwrap();
        let rate = capture.format().sample_rate;
        let mut source = StreamSource::spawn(capture).unwrap();
        let defaults = Config::default();
        let mut analyzer = Analyzer::new(&defaults.analysis, &defaults.general, rate);
        let mut samples = Vec::new();
        let mut received = 0;
        let mut features = crate::dsp::AudioFeatures::default();
        for _ in 0..180 {
            samples.clear();
            source.pull(1.0 / 60.0, &mut samples).unwrap();
            received += samples.len();
            analyzer.push(&samples);
            features = *analyzer.analyze(1.0 / 60.0);
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        println!(
            "rate {rate} Hz, {received} samples in 3 s, energy {:.2}, pitch {:?} octave {}",
            features.energy, features.pitch_class, features.octave
        );
        assert!(source.connected());
        assert_eq!((features.pitch_class, features.octave), (Some(9), 4));
    }
}
