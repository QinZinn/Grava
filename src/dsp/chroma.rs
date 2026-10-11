//! Pitch-class (chroma) accumulation, smoothing, and dominant-pitch selection.
//!
//! The chroma vector folds spectral peaks into twelve pitch classes weighted by
//! magnitude squared. The analyzer hands it the strongest peaks of the frame
//! (up to 32, above a relative threshold) with *interpolated* frequencies
//! rather than raw FFT bins:
//! below about 200 Hz a bin (11.7 Hz at 4096 samples and 48 kHz) is wider than
//! a semitone, so bin centres can straddle a note and never name it.

use super::peaks::Peak;

/// Fold peaks between these frequencies (intersected with the analysis range).
pub const MIN_HZ: f32 = 55.0;
pub const MAX_HZ: f32 = 2000.0;
/// Smoothing time constant of the chroma average, in seconds.
pub const SMOOTHING: f32 = 0.15;

/// MIDI note number (69 = A4 = 440 Hz) of a frequency, fractional.
pub fn midi_from_hz(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

/// Pitch class (0 = C) and octave (C4 = middle C = octave 4) of a frequency.
pub fn note_from_hz(hz: f32) -> (u8, i8) {
    let midi = midi_from_hz(hz).round() as i32;
    (
        midi.rem_euclid(12) as u8,
        (midi.div_euclid(12) - 1).clamp(-128, 127) as i8,
    )
}

/// The dominant pitch of the smoothed chroma.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pitch {
    pub class: u8,
    pub octave: i8,
    /// Share of the chroma energy in the dominant class: about 1/12 for noise,
    /// near 1 for a clean tone.
    pub confidence: f32,
    /// Frequency of the strongest peak that belongs to the class.
    pub hz: f32,
}

#[derive(Debug, Clone)]
pub struct Chroma {
    min_hz: f32,
    max_hz: f32,
    frame: [f32; 12],
    /// Strongest peak of each class in the current frame.
    strongest: [Option<Peak>; 12],
    /// The most recent frequency seen for each class, kept while a class
    /// lingers in the smoothed average after its peak has gone.
    remembered: [Option<f32>; 12],
    smoothed: [f32; 12],
    primed: bool,
}

impl Chroma {
    /// `low` and `high` are the analysis cutoffs; the fold range is their
    /// intersection with `MIN_HZ..MAX_HZ`.
    pub fn new(low: f32, high: f32) -> Self {
        Self {
            min_hz: low.max(MIN_HZ),
            max_hz: high.min(MAX_HZ),
            frame: [0.0; 12],
            strongest: [None; 12],
            remembered: [None; 12],
            smoothed: [0.0; 12],
            primed: false,
        }
    }

    pub fn begin_frame(&mut self) {
        self.frame = [0.0; 12];
        self.strongest = [None; 12];
    }

    pub fn add(&mut self, peak: Peak) {
        if !(self.min_hz..=self.max_hz).contains(&peak.hz) || !peak.magnitude.is_finite() {
            return;
        }
        let (class, _) = note_from_hz(peak.hz);
        let class = usize::from(class);
        self.frame[class] += peak.magnitude * peak.magnitude;
        if self.strongest[class].is_none_or(|best| peak.magnitude > best.magnitude) {
            self.strongest[class] = Some(peak);
        }
    }

    /// Fold the frame into the running average. A frame without any pitched
    /// energy leaves the average untouched.
    pub fn end_frame(&mut self, dt: f32) {
        let total: f32 = self.frame.iter().sum();
        if !total.is_finite() || total <= 1e-12 {
            return;
        }
        let blend = if self.primed && dt.is_finite() && dt > 0.0 {
            1.0 - (-dt / SMOOTHING).exp()
        } else {
            1.0
        };
        self.primed = true;
        for class in 0..12 {
            self.smoothed[class] += (self.frame[class] / total - self.smoothed[class]) * blend;
            if let Some(peak) = self.strongest[class] {
                self.remembered[class] = Some(peak.hz);
            }
        }
    }

    pub fn smoothed(&self) -> &[f32; 12] {
        &self.smoothed
    }

    pub fn dominant(&self) -> Option<Pitch> {
        if !self.primed {
            return None;
        }
        let mut class = 0;
        for candidate in 1..12 {
            if self.smoothed[candidate] > self.smoothed[class] {
                class = candidate;
            }
        }
        let hz = self.remembered[class]?;
        let (_, octave) = note_from_hz(hz);
        Some(Pitch {
            class: class as u8,
            octave,
            confidence: self.smoothed[class].clamp(0.0, 1.0),
            hz,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn peak(hz: f32, magnitude: f32) -> Peak {
        Peak { hz, magnitude }
    }

    #[test]
    fn frequencies_map_to_pitch_classes_and_octaves() {
        for (hz, class, octave) in [
            (440.0, 9, 4),
            (261.63, 0, 4),
            (130.81, 0, 3),
            (82.41, 4, 2),
            (65.41, 0, 2),
            (61.74, 11, 1),
            (1046.5, 0, 6),
            (466.16, 10, 4),
            (452.0, 9, 4),
            (428.0, 9, 4),
        ] {
            assert_eq!(note_from_hz(hz), (class, octave), "{hz} Hz");
        }
        assert!((midi_from_hz(440.0) - 69.0).abs() < 1e-5);
    }

    #[test]
    fn dominant_class_octave_and_confidence() {
        let mut chroma = Chroma::new(40.0, 8000.0);
        assert!(chroma.dominant().is_none());
        chroma.begin_frame();
        chroma.add(peak(220.0, 0.5));
        chroma.add(peak(440.0, 0.25));
        chroma.add(peak(660.0, 0.1));
        chroma.end_frame(1.0 / 60.0);
        let pitch = chroma.dominant().unwrap();
        // The strongest A peak is the 220 Hz fundamental, not its octave.
        assert_eq!((pitch.class, pitch.octave), (9, 3));
        assert!((pitch.hz - 220.0).abs() < 1e-3);
        assert!(pitch.confidence > 0.7, "{}", pitch.confidence);
        assert!((chroma.smoothed().iter().sum::<f32>() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn peaks_outside_the_fold_range_are_ignored() {
        let mut chroma = Chroma::new(40.0, 8000.0);
        chroma.begin_frame();
        chroma.add(peak(40.0, 1.0));
        chroma.add(peak(3000.0, 1.0));
        chroma.add(peak(f32::NAN, 1.0));
        chroma.add(peak(440.0, f32::NAN));
        chroma.end_frame(1.0 / 60.0);
        assert!(chroma.dominant().is_none());
        let mut narrow = Chroma::new(300.0, 500.0);
        narrow.begin_frame();
        narrow.add(peak(220.0, 1.0));
        narrow.add(peak(440.0, 0.1));
        narrow.end_frame(1.0 / 60.0);
        assert_eq!(narrow.dominant().unwrap().class, 9);
    }

    #[test]
    fn smoothing_needs_time_and_empty_frames_keep_the_average() {
        let mut chroma = Chroma::new(40.0, 8000.0);
        let frame = |chroma: &mut Chroma, hz| {
            chroma.begin_frame();
            chroma.add(peak(hz, 0.5));
            chroma.end_frame(1.0 / 60.0);
        };
        for _ in 0..30 {
            frame(&mut chroma, 261.63);
        }
        assert_eq!(chroma.dominant().unwrap().class, 0);
        // After a note change, A overtakes C only after about 0.15 * ln 2 s.
        let mut frames_to_switch = 0;
        while chroma.dominant().unwrap().class == 0 {
            frame(&mut chroma, 440.0);
            frames_to_switch += 1;
            assert!(frames_to_switch < 60);
        }
        assert!((4..=10).contains(&frames_to_switch), "{frames_to_switch}");
        let before = *chroma.smoothed();
        chroma.begin_frame();
        chroma.end_frame(1.0 / 60.0);
        assert_eq!(*chroma.smoothed(), before);
    }

    #[test]
    fn a_triad_has_lower_confidence_than_a_single_tone() {
        let mut single = Chroma::new(40.0, 8000.0);
        let mut triad = Chroma::new(40.0, 8000.0);
        single.begin_frame();
        single.add(peak(261.63, 0.3));
        single.end_frame(0.016);
        triad.begin_frame();
        for hz in [261.63, 329.63, 392.0] {
            triad.add(peak(hz, 0.3));
        }
        triad.end_frame(0.016);
        let (single, triad) = (single.dominant().unwrap(), triad.dominant().unwrap());
        assert!(single.confidence > 0.99);
        assert!((triad.confidence - 1.0 / 3.0).abs() < 0.01);
    }
}
