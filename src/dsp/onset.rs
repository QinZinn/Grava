//! Onset detection by spectral flux on log magnitudes.
//!
//! The flux of a frame is the mean positive change of the log-compressed
//! spectrum since the previous frame. An onset fires when the flux is still
//! rising and exceeds an adaptive threshold (mean plus a multiple of the
//! standard deviation over the last second), and not within the refractory
//! period of the previous onset.

/// Log compression `ln(1 + COMPRESSION * gain * magnitude)`: quiet partials
/// count as much as loud ones in a broadband attack.
const COMPRESSION: f32 = 1000.0;
/// Multiples of the standard deviation above the mean needed to fire.
const DEVIATIONS: f32 = 1.5;
/// The threshold sits at least this far above the mean, so that a perfectly
/// steady tone (zero deviation) cannot fire on rounding noise.
const MIN_RISE: f32 = 0.05;
/// Seconds after an onset during which no other can fire.
const REFRACTORY: f32 = 0.1;
/// Frames of history needed before any onset may fire.
const WARM_UP: usize = 8;

#[derive(Debug, Clone)]
pub struct OnsetDetector {
    previous: Vec<f32>,
    primed: bool,
    history: Vec<f32>,
    cursor: usize,
    filled: usize,
    last_flux: f32,
    since_onset: f32,
}

impl OnsetDetector {
    /// `bins` is the length of the spectrum slice passed to [`Self::process`];
    /// `history_frames` is about one second of frames.
    pub fn new(bins: usize, history_frames: usize) -> Self {
        Self {
            previous: vec![0.0; bins],
            primed: false,
            history: vec![0.0; history_frames.max(WARM_UP + 1)],
            cursor: 0,
            filled: 0,
            last_flux: 0.0,
            since_onset: REFRACTORY,
        }
    }

    /// Return the onset strength in `0..=1`; zero on every other frame.
    /// `gain` is the loudness gain, which keeps the compression level-invariant.
    pub fn process(&mut self, magnitudes: &[f32], gain: f32, dt: f32) -> f32 {
        debug_assert_eq!(magnitudes.len(), self.previous.len());
        if self.previous.is_empty() {
            return 0.0;
        }
        if dt.is_finite() && dt > 0.0 {
            self.since_onset += dt;
        }
        let scale = COMPRESSION * gain.max(0.0);
        let mut rise = 0.0;
        for (previous, &magnitude) in self.previous.iter_mut().zip(magnitudes) {
            let current = (1.0 + scale * magnitude).ln();
            let difference = current - *previous;
            if difference > 0.0 {
                rise += difference;
            }
            *previous = current;
        }
        let flux = if self.primed {
            rise / self.previous.len() as f32
        } else {
            // The first spectrum has nothing to compare with.
            self.primed = true;
            0.0
        };

        let (mean, deviation) = self.statistics();
        let strength = if self.filled >= WARM_UP
            && flux > mean + (DEVIATIONS * deviation).max(MIN_RISE)
            && flux > self.last_flux
            && self.since_onset >= REFRACTORY
        {
            self.since_onset = 0.0;
            ((flux - mean) / (4.0 * deviation.max(MIN_RISE))).clamp(0.0, 1.0)
        } else {
            0.0
        };

        self.history[self.cursor] = flux;
        self.cursor = (self.cursor + 1) % self.history.len();
        self.filled = (self.filled + 1).min(self.history.len());
        self.last_flux = flux;
        strength
    }

    fn statistics(&self) -> (f32, f32) {
        if self.filled == 0 {
            return (0.0, 0.0);
        }
        let values = &self.history[..self.filled.min(self.history.len())];
        let count = values.len() as f32;
        let mean = values.iter().sum::<f32>() / count;
        let variance = values
            .iter()
            .map(|value| (value - mean) * (value - mean))
            .sum::<f32>()
            / count;
        (mean, variance.sqrt())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn spectrum(level: f32, bins: usize) -> Vec<f32> {
        vec![level; bins]
    }

    #[test]
    fn steady_and_slowly_wandering_spectra_never_fire() {
        let mut detector = OnsetDetector::new(64, 60);
        let mut rng = fastrand::Rng::with_seed(5);
        for frame in 0..600 {
            // +-3 % level wobble, like the residual of a steady tone.
            let wobble = 1.0 + 0.03 * (rng.f32() - 0.5);
            let strength = detector.process(&spectrum(0.01 * wobble, 64), 3.0, DT);
            assert_eq!(strength, 0.0, "fired on frame {frame}");
        }
    }

    #[test]
    fn broadband_jumps_fire_once_each_with_strength() {
        let mut detector = OnsetDetector::new(64, 60);
        let mut fired = Vec::new();
        for frame in 0..300 {
            // A burst every 30 frames that decays over five frames.
            let since = frame % 30;
            let level = 0.005
                + if since < 5 {
                    0.2 * (1.0 - since as f32 / 5.0)
                } else {
                    0.0
                };
            let strength = detector.process(&spectrum(level, 64), 3.0, DT);
            if strength > 0.0 {
                fired.push((frame, strength));
            }
        }
        // The first burst falls inside the warm-up; the rest fire promptly.
        let frames: Vec<_> = fired.iter().map(|&(frame, _)| frame % 30).collect();
        assert!(fired.len() >= 8 && fired.len() <= 10, "{fired:?}");
        assert!(frames.iter().all(|&offset| offset <= 1), "{frames:?}");
        assert!(
            fired
                .iter()
                .all(|&(_, strength)| (0.2..=1.0).contains(&strength))
        );
    }

    #[test]
    fn refractory_period_blocks_a_second_onset_inside_100_ms() {
        let mut detector = OnsetDetector::new(32, 60);
        for _ in 0..30 {
            detector.process(&spectrum(0.005, 32), 3.0, DT);
        }
        let mut fires = 0;
        // Two rises three frames (50 ms) apart.
        for level in [0.3, 0.005, 0.005, 0.5, 0.005, 0.005, 0.005] {
            if detector.process(&spectrum(level, 32), 3.0, DT) > 0.0 {
                fires += 1;
            }
        }
        assert_eq!(fires, 1);
        for _ in 0..10 {
            detector.process(&spectrum(0.005, 32), 3.0, DT);
        }
        assert!(detector.process(&spectrum(0.4, 32), 3.0, DT) > 0.0);
    }

    #[test]
    fn empty_spectra_and_bad_timesteps_are_harmless() {
        let mut empty = OnsetDetector::new(0, 60);
        assert_eq!(empty.process(&[], 1.0, DT), 0.0);
        let mut detector = OnsetDetector::new(4, 60);
        for dt in [f32::NAN, -1.0, 0.0, f32::INFINITY] {
            assert!(detector.process(&[0.1; 4], 1.0, dt) >= 0.0);
        }
        assert!(detector.process(&[0.1; 4], f32::NAN, DT) >= 0.0);
    }
}
