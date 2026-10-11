//! Per-frame audio analysis: spectrum, loudness, peaks, pitch, and onsets.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex, num_complex::Complex};

use crate::config::schema::{AnalysisConfig, GeneralConfig};

use super::{
    chroma::Chroma,
    loudness::Loudness,
    onset::OnsetDetector,
    peaks::{self, MAX_CHROMA_PEAKS, MAX_PEAKS, TopPeaks},
};

/// Peaks quieter than this (about -80 dBFS) are ignored as numerical dust.
const PEAK_FLOOR: f32 = 1e-4;
/// Peaks weaker than this fraction of the strongest are ignored, both for the
/// chroma and for the reported peaks.
const RELATIVE_PEAK_FLOOR: f32 = 0.05;

/// Everything the visualization needs to know about the last frame of audio.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AudioFeatures {
    /// 0 = silent, ~0.6 typical, up to ~1.5 (never more than 2).
    pub energy: f32,
    /// 0..1, non-zero only on onset frames.
    pub onset: f32,
    /// Smoothed pitch-class distribution; sums to 1 once any pitch was heard.
    pub chroma: [f32; 12],
    /// 0 = C ... 11 = B; `None` in silence or before any pitch was heard.
    pub pitch_class: Option<u8>,
    /// Octave of the dominant pitch (C4 = middle C = 4).
    pub octave: i8,
    /// 0..1: the share of chroma energy in the dominant class.
    pub confidence: f32,
    /// Frequency of the strongest peak of the dominant class.
    pub dominant_hz: Option<f32>,
    /// `(hz, magnitude)` of the strongest spectral peaks, strongest first.
    pub peaks: [(f32, f32); MAX_PEAKS],
    pub peak_count: usize,
}

impl AudioFeatures {
    /// The valid prefix of [`Self::peaks`].
    pub fn peak_slice(&self) -> &[(f32, f32)] {
        &self.peaks[..self.peak_count.min(MAX_PEAKS)]
    }
}

/// Windowed FFT analysis of the latest `fft_size` samples. All buffers are
/// allocated in [`Analyzer::new`]; `push` and `analyze` never allocate.
pub struct Analyzer {
    bin_hz: f32,
    mask: usize,
    window: Vec<f32>,
    ring: Vec<f32>,
    write: usize,
    frame: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    magnitude: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
    /// Inclusive bin range scanned for peaks and flux; empty if first > last.
    first_bin: usize,
    last_bin: usize,
    loudness: Loudness,
    chroma: Chroma,
    onset: OnsetDetector,
    sum_squares: f64,
    new_samples: usize,
    features: AudioFeatures,
}

impl Analyzer {
    /// `sample_rate` is in Hz. The cutoffs are clipped to the available bins.
    pub fn new(analysis: &AnalysisConfig, general: &GeneralConfig, sample_rate: u32) -> Self {
        let size = analysis.fft_size.max(2).next_power_of_two();
        let rate = sample_rate.max(1) as f32;
        let bin_hz = rate / size as f32;
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(size);
        let window = (0..size)
            .map(|index| 0.5 - 0.5 * (std::f32::consts::TAU * index as f32 / size as f32).cos())
            .collect();
        // Peak search reads one bin on either side, so stay inside 1..=N/2-1.
        let first_bin = ((analysis.low_cutoff / bin_hz).ceil().max(1.0)) as usize;
        let last_bin = ((analysis.high_cutoff / bin_hz).floor() as usize).min(size / 2 - 1);
        let band = if last_bin >= first_bin {
            last_bin - first_bin + 1
        } else {
            0
        };
        let history = general.framerate.max(1) as usize;
        Self {
            bin_hz,
            mask: size - 1,
            window,
            ring: vec![0.0; size],
            write: 0,
            frame: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            magnitude: vec![0.0; size / 2 + 1],
            fft,
            first_bin,
            last_bin,
            loudness: Loudness::new(
                analysis.attack_ms as f32 / 1000.0,
                analysis.release_ms as f32 / 1000.0,
                general.autosens,
                general.sensitivity,
                analysis.noise_gate,
            ),
            chroma: Chroma::new(analysis.low_cutoff, analysis.high_cutoff.min(rate / 2.0)),
            onset: OnsetDetector::new(band, history),
            sum_squares: 0.0,
            new_samples: 0,
            features: AudioFeatures::default(),
        }
    }

    pub fn fft_size(&self) -> usize {
        self.mask + 1
    }

    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.loudness.set_sensitivity(sensitivity);
    }

    pub fn sensitivity(&self) -> f32 {
        self.loudness.sensitivity()
    }

    pub fn features(&self) -> &AudioFeatures {
        &self.features
    }

    /// Append mono samples in -1..1. Non-finite samples count as silence.
    pub fn push(&mut self, samples: &[f32]) {
        for &sample in samples {
            let sample = if sample.is_finite() { sample } else { 0.0 };
            self.ring[self.write] = sample;
            self.write = (self.write + 1) & self.mask;
            self.sum_squares += f64::from(sample) * f64::from(sample);
        }
        self.new_samples += samples.len();
    }

    /// Analyze the newest window; `dt` is the time since the previous call.
    ///
    /// Loudness covers the samples pushed since the last call. In silence the
    /// FFT is skipped and the previous chroma is kept: a plate at rest keeps
    /// its figure.
    pub fn analyze(&mut self, dt: f32) -> &AudioFeatures {
        let energy = self
            .loudness
            .process(self.sum_squares, self.new_samples, dt);
        self.sum_squares = 0.0;
        self.new_samples = 0;
        self.features.energy = energy;
        self.features.chroma = *self.chroma.smoothed();
        if energy <= 0.0 {
            self.features.onset = 0.0;
            self.features.pitch_class = None;
            self.features.confidence = 0.0;
            self.features.dominant_hz = None;
            self.features.octave = 0;
            self.features.peak_count = 0;
            return &self.features;
        }

        // Oldest sample first: the ring's write cursor is the oldest slot.
        for (index, (frame, window)) in self.frame.iter_mut().zip(&self.window).enumerate() {
            *frame = self.ring[(self.write + index) & self.mask] * window;
        }
        // The buffers come from the planner, so the lengths cannot mismatch.
        let transformed =
            self.fft
                .process_with_scratch(&mut self.frame, &mut self.spectrum, &mut self.scratch);
        debug_assert!(transformed.is_ok());
        if transformed.is_err() {
            self.features.onset = 0.0;
            return &self.features;
        }
        // A sine of amplitude A peaks at A * N / 4 under a Hann window.
        let scale = 4.0 / self.fft_size() as f32;
        for (magnitude, value) in self.magnitude.iter_mut().zip(&self.spectrum) {
            *magnitude = value.norm() * scale;
        }

        // The chroma folds the strongest peaks above the threshold, which keeps
        // the hundreds of weak local maxima of noise out of the pitch estimate.
        // Only the first few are reported as `features.peaks`, for the
        // spectrum mapping.
        let mut top = TopPeaks::<MAX_CHROMA_PEAKS>::default();
        peaks::scan(
            &self.magnitude,
            self.first_bin,
            self.last_bin,
            self.bin_hz,
            PEAK_FLOOR,
            |peak| top.insert(peak),
        );
        top.retain_relative(RELATIVE_PEAK_FLOOR);
        self.chroma.begin_frame();
        for &peak in top.as_slice() {
            self.chroma.add(peak);
        }
        self.chroma.end_frame(dt);

        self.features.onset = if self.first_bin <= self.last_bin {
            self.onset.process(
                &self.magnitude[self.first_bin..=self.last_bin],
                self.loudness.gain(),
                dt,
            )
        } else {
            0.0
        };
        self.features.chroma = *self.chroma.smoothed();
        match self.chroma.dominant() {
            Some(pitch) => {
                self.features.pitch_class = Some(pitch.class);
                self.features.octave = pitch.octave;
                self.features.confidence = pitch.confidence;
                self.features.dominant_hz = Some(pitch.hz);
            }
            None => {
                self.features.pitch_class = None;
                self.features.octave = 0;
                self.features.confidence = 0.0;
                self.features.dominant_hz = None;
            }
        }
        self.features.peaks = [(0.0, 0.0); MAX_PEAKS];
        let reported = &top.as_slice()[..top.as_slice().len().min(MAX_PEAKS)];
        for (slot, peak) in self.features.peaks.iter_mut().zip(reported) {
            *slot = (peak.hz, peak.magnitude);
        }
        self.features.peak_count = reported.len();
        &self.features
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn analyzer(rate: u32) -> Analyzer {
        Analyzer::new(&AnalysisConfig::default(), &GeneralConfig::default(), rate)
    }

    /// Feed `seconds` of a signal in 60 fps frames, returning the final features
    /// and the number of onset frames.
    fn run(
        analyzer: &mut Analyzer,
        rate: u32,
        seconds: f32,
        mut signal: impl FnMut(f32) -> f32,
    ) -> (AudioFeatures, usize) {
        let mut sample = 0_u64;
        let mut onsets = 0;
        let mut last = AudioFeatures::default();
        let mut chunk = Vec::new();
        for _ in 0..(seconds * 60.0) as usize {
            chunk.clear();
            let end = ((sample as f64 / f64::from(rate) + f64::from(DT)) * f64::from(rate)) as u64;
            while sample < end {
                chunk.push(signal(sample as f32 / rate as f32));
                sample += 1;
            }
            analyzer.push(&chunk);
            last = *analyzer.analyze(DT);
            onsets += usize::from(last.onset > 0.0);
        }
        (last, onsets)
    }

    fn sine(hz: f32, amplitude: f32) -> impl FnMut(f32) -> f32 {
        move |t| amplitude * (std::f32::consts::TAU * hz * t).sin()
    }

    #[test]
    fn a_440_hz_sine_is_a4() {
        for rate in [44_100, 48_000, 96_000] {
            let mut analyzer = analyzer(rate);
            let (features, onsets) = run(&mut analyzer, rate, 1.5, sine(440.0, 0.3));
            assert_eq!(features.pitch_class, Some(9), "{rate} Hz");
            assert_eq!(features.octave, 4);
            assert!((features.dominant_hz.unwrap() - 440.0).abs() < 1.5);
            assert!(features.confidence > 0.9, "{}", features.confidence);
            assert!(features.energy > 0.3 && features.energy < 1.5);
            assert!(features.peak_count >= 1);
            assert!((features.peaks[0].0 - 440.0).abs() < 1.5);
            // A pure tone is not a rhythm: at most the very first attack fires.
            assert!(onsets <= 1, "{onsets} onsets");
            assert!((features.chroma.iter().sum::<f32>() - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn low_and_high_notes_get_the_right_class_and_octave() {
        for (hz, class, octave) in [
            (110.0, 9, 2),
            (82.41, 4, 2),
            (98.0, 7, 2),
            (65.41, 0, 2),
            (130.81, 0, 3),
            (493.88, 11, 4),
            (1046.5, 0, 6),
        ] {
            let mut analyzer = analyzer(48_000);
            let (features, _) = run(&mut analyzer, 48_000, 1.5, sine(hz, 0.3));
            assert_eq!(
                (features.pitch_class, features.octave),
                (Some(class), octave),
                "{hz} Hz -> {:?} {}",
                features.pitch_class,
                features.octave
            );
        }
    }

    #[test]
    fn harmonic_tones_report_their_fundamental() {
        let mut analyzer = analyzer(48_000);
        let signal = |t: f32| {
            [1.0_f32, 0.5, 0.33, 0.2]
                .iter()
                .enumerate()
                .map(|(index, amplitude)| {
                    0.15 * amplitude
                        * (std::f32::consts::TAU * 220.0 * (index + 1) as f32 * t).sin()
                })
                .sum()
        };
        let (features, _) = run(&mut analyzer, 48_000, 1.5, signal);
        assert_eq!((features.pitch_class, features.octave), (Some(9), 3));
        assert!(features.peak_count >= 3);
        assert!((features.peaks[0].0 - 220.0).abs() < 1.5);
    }

    #[test]
    fn noise_is_unpitched_and_silence_keeps_the_last_chroma() {
        let mut analyzer = analyzer(48_000);
        let mut rng = fastrand::Rng::with_seed(11);
        let (noisy, _) = run(&mut analyzer, 48_000, 2.0, |_| {
            0.3 * (rng.f32() * 2.0 - 1.0)
        });
        assert!(noisy.confidence < 0.25, "{}", noisy.confidence);

        let mut analyzer = self::analyzer(48_000);
        let (tone, _) = run(&mut analyzer, 48_000, 1.0, sine(440.0, 0.3));
        // Cutting a tone dead is itself a click; once the tail has decayed,
        // silence must stay silent.
        run(&mut analyzer, 48_000, 2.0, |_| 0.0);
        let (silent, onsets) = run(&mut analyzer, 48_000, 3.0, |_| 0.0);
        assert_eq!(silent.energy, 0.0);
        assert_eq!((silent.pitch_class, silent.dominant_hz), (None, None));
        assert_eq!((silent.peak_count, silent.onset), (0, 0.0));
        assert_eq!(onsets, 0);
        assert_eq!(silent.chroma, tone.chroma);
    }

    #[test]
    fn clicks_fire_onsets_but_a_steady_tone_does_not() {
        // A 40 Hz-wide kick-like burst every half second over a quiet pad.
        let mut analyzer = analyzer(48_000);
        let (_, onsets) = run(&mut analyzer, 48_000, 6.0, |t| {
            let phase = t % 0.5;
            let burst = if phase < 0.06 {
                0.7 * (-phase / 0.015).exp()
                    * (std::f32::consts::TAU * (60.0 + 800.0 * phase) * phase).sin()
            } else {
                0.0
            };
            burst + 0.02 * (std::f32::consts::TAU * 330.0 * t).sin()
        });
        // Twelve bursts; the first lands while the detector warms up.
        assert!((10..=13).contains(&onsets), "{onsets} onsets");

        let mut steady = self::analyzer(48_000);
        let (_, onsets) = run(&mut steady, 48_000, 6.0, sine(330.0, 0.3));
        assert!(onsets <= 1, "{onsets} onsets on a steady tone");
    }

    #[test]
    fn hostile_samples_and_configurations_do_not_panic() {
        let mut analyzer = analyzer(48_000);
        analyzer.push(&[f32::NAN, f32::INFINITY, -f32::INFINITY, 1e30, -1e30]);
        let features = *analyzer.analyze(DT);
        assert!(features.energy.is_finite() && features.confidence.is_finite());
        analyzer.push(&vec![0.5; 100_000]);
        assert!(analyzer.analyze(f32::NAN).energy.is_finite());
        assert!(analyzer.analyze(0.0).energy.is_finite());
        for size in [2, 4, 64, 1000, 65_536] {
            for rate in [1, 8_000, 192_000] {
                let config = AnalysisConfig {
                    fft_size: size,
                    ..AnalysisConfig::default()
                };
                let mut small = Analyzer::new(&config, &GeneralConfig::default(), rate);
                small.push(&vec![0.4; 4096]);
                let features = small.analyze(DT);
                assert!(features.energy.is_finite());
                assert!(small.fft_size().is_power_of_two());
            }
        }
        let inverted = AnalysisConfig {
            low_cutoff: 9000.0,
            high_cutoff: 9100.0,
            ..AnalysisConfig::default()
        };
        let mut empty_band = Analyzer::new(&inverted, &GeneralConfig::default(), 8_000);
        empty_band.push(&[0.5; 512]);
        assert_eq!(empty_band.analyze(DT).peak_count, 0);
    }

    #[test]
    fn sensitivity_scales_energy_without_the_gain_chasing_it() {
        let mut analyzer = analyzer(48_000);
        run(&mut analyzer, 48_000, 20.0, sine(440.0, 0.1));
        let normal = analyzer.features().energy;
        analyzer.set_sensitivity(2.0);
        let (louder, _) = run(&mut analyzer, 48_000, 0.5, sine(440.0, 0.1));
        assert!(
            louder.energy > normal * 1.7,
            "{normal} -> {}",
            louder.energy
        );
        assert_eq!(analyzer.sensitivity(), 2.0);
    }
}
