//! Loudness: RMS, attack/release smoothing, automatic gain, and a noise gate.
//!
//! The automatic gain follows the idea of cava's `autosens` (raise the gain
//! slowly while the signal is quiet, cut it quickly on overshoot), but works
//! on RMS energy in continuous time rather than per bar.

/// Gain applied when `autosens` is off, and the starting gain when it is on.
/// A typical master at about -14 dBFS RMS (0.2) then lands near 0.6.
pub const REFERENCE_GAIN: f32 = 3.0;
/// The energy the gain control settles towards while the signal is quiet.
const TARGET_ENERGY: f32 = 0.6;
/// Above this, the gain is cut at once.
const OVERSHOOT_ENERGY: f32 = 1.0;
/// Gain change per second: fast attack, slow recovery.
const CUT_RATE: f32 = 2.5;
const RAISE_RATE: f32 = 0.2;
const MIN_GAIN: f32 = 0.2;
const MAX_GAIN: f32 = 100.0;
/// Below this smoothed RMS (about -60 dBFS) the input is silence: the energy
/// is zero and the gain stays put so that hiss is never amplified.
const SILENCE_RMS: f32 = 1e-3;
/// The gate closes below this fraction of its opening threshold.
const GATE_HYSTERESIS: f32 = 0.7;
const MAX_ENERGY: f32 = 2.0;

#[derive(Debug, Clone)]
pub struct Loudness {
    attack: f32,
    release: f32,
    autosens: bool,
    sensitivity: f32,
    gate: f32,
    level: f32,
    gain: f32,
    open: bool,
}

impl Loudness {
    /// Times are in seconds; `gate` is the energy below which the output is zero.
    pub fn new(attack: f32, release: f32, autosens: bool, sensitivity: f32, gate: f32) -> Self {
        Self {
            attack: attack.max(0.0),
            release: release.max(0.0),
            autosens,
            sensitivity: sensitivity.max(0.0),
            gate: gate.max(0.0),
            level: 0.0,
            gain: REFERENCE_GAIN,
            open: false,
        }
    }

    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.sensitivity = sensitivity.max(0.0);
    }

    /// The automatic gain before `sensitivity`; scales spectra for onset detection.
    pub fn gain(&self) -> f32 {
        if self.autosens {
            self.gain
        } else {
            REFERENCE_GAIN
        }
    }

    pub fn sensitivity(&self) -> f32 {
        self.sensitivity
    }

    /// The smoothed RMS before any gain.
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Advance by `dt` seconds with the sum of squares of `count` new samples
    /// and return the energy: 0 silent, about 0.6 typical, at most 2.
    pub fn process(&mut self, sum_squares: f64, count: usize, dt: f32) -> f32 {
        let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        let rms = if count > 0 {
            (sum_squares / count as f64).sqrt() as f32
        } else {
            0.0
        };
        let rms = if rms.is_finite() { rms } else { 0.0 };
        let time_constant = if rms > self.level {
            self.attack
        } else {
            self.release
        };
        self.level = if dt == 0.0 {
            self.level
        } else if time_constant <= 0.0 {
            rms
        } else {
            self.level + (rms - self.level) * (1.0 - (-dt / time_constant).exp())
        };
        if !self.level.is_finite() || self.level < SILENCE_RMS {
            self.open = false;
            return 0.0;
        }
        let mut gain = REFERENCE_GAIN;
        if self.autosens {
            let raw = self.level * self.gain;
            if raw > OVERSHOOT_ENERGY {
                self.gain *= (-CUT_RATE * dt).exp();
            } else if raw < TARGET_ENERGY && rms >= SILENCE_RMS {
                // Only while sound is arriving: the decaying tail of a note
                // that has ended must not inflate the gain.
                self.gain *= (RAISE_RATE * dt).exp();
            }
            self.gain = self.gain.clamp(MIN_GAIN, MAX_GAIN);
            gain = self.gain;
        }
        let energy = self.level * gain * self.sensitivity;
        let threshold = if self.open {
            self.gate * GATE_HYSTERESIS
        } else {
            self.gate
        };
        self.open = energy >= threshold && energy > 0.0;
        if self.open {
            energy.min(MAX_ENERGY)
        } else {
            0.0
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn run(loudness: &mut Loudness, rms: f32, seconds: f32) -> f32 {
        let mut energy = 0.0;
        for _ in 0..(seconds / DT) as usize {
            // 800 samples per frame, all of amplitude `rms`.
            energy = loudness.process(f64::from(rms * rms) * 800.0, 800, DT);
        }
        energy
    }

    #[test]
    fn typical_levels_settle_near_the_target_and_loud_levels_are_cut() {
        for rms in [0.02_f32, 0.07, 0.2, 0.5] {
            let mut loudness = Loudness::new(0.02, 0.25, true, 1.0, 0.02);
            let energy = run(&mut loudness, rms, 40.0);
            assert!(
                (0.5..=1.0).contains(&energy),
                "rms {rms} settled at {energy}"
            );
        }
    }

    #[test]
    fn overshoot_is_cut_quickly_and_quiet_recovers_slowly() {
        let mut loudness = Loudness::new(0.02, 0.25, true, 1.0, 0.02);
        run(&mut loudness, 0.2, 20.0);
        let settled = loudness.gain();
        // A sudden 4x louder passage briefly exceeds the target, then is tamed.
        let first = loudness.process(f64::from(0.8_f32 * 0.8) * 800.0, 800, DT);
        run(&mut loudness, 0.8, 0.5);
        assert!(loudness.gain() < settled * 0.5, "{}", loudness.gain());
        assert!(first < 2.01);
        let after_cut = loudness.gain();
        run(&mut loudness, 0.2, 1.0);
        assert!(loudness.gain() < after_cut * 1.3, "raise must be slow");
    }

    #[test]
    fn attack_is_faster_than_release() {
        let mut loudness = Loudness::new(0.02, 0.25, false, 1.0, 0.0);
        run(&mut loudness, 0.3, 1.0);
        let full = loudness.level();
        // One frame of silence barely moves a 250 ms release...
        loudness.process(0.0, 800, DT);
        assert!(loudness.level() > full * 0.9);
        // ...while the first frame of a note already reaches most of its level.
        let mut fresh = Loudness::new(0.02, 0.25, false, 1.0, 0.0);
        fresh.process(f64::from(0.3_f32 * 0.3) * 800.0, 800, DT);
        assert!(fresh.level() > 0.3 * 0.5);
    }

    #[test]
    fn silence_gates_to_zero_and_never_inflates_the_gain() {
        let mut loudness = Loudness::new(0.02, 0.25, true, 1.0, 0.02);
        run(&mut loudness, 0.2, 5.0);
        let gain = loudness.gain();
        assert_eq!(run(&mut loudness, 0.0, 60.0), 0.0);
        assert_eq!(loudness.gain(), gain);
        // Hiss at -70 dBFS is silence too.
        assert_eq!(run(&mut loudness, 3e-4, 10.0), 0.0);
        assert_eq!(loudness.gain(), gain);
    }

    #[test]
    fn gate_applies_after_gain_with_hysteresis() {
        let mut loudness = Loudness::new(0.0, 0.0, false, 1.0, 0.3);
        // Energy 0.2 * 3 = 0.6 passes; 0.09 * 3 = 0.27 is below 0.3 ...
        assert!(run(&mut loudness, 0.2, 0.1) > 0.5);
        assert_eq!(run(&mut loudness, 0.05, 0.1), 0.0);
        // ... but once open, 0.27 stays above the closing threshold 0.21.
        let mut open = Loudness::new(0.0, 0.0, false, 1.0, 0.3);
        run(&mut open, 0.2, 0.1);
        assert!(run(&mut open, 0.09, 0.1) > 0.2);
    }

    #[test]
    fn fixed_gain_uses_sensitivity_only_and_energy_is_bounded() {
        let mut loudness = Loudness::new(0.0, 0.0, false, 2.0, 0.0);
        let energy = run(&mut loudness, 0.1, 1.0);
        assert!((energy - 0.6).abs() < 1e-3, "{energy}");
        loudness.set_sensitivity(0.0);
        assert_eq!(run(&mut loudness, 0.1, 0.1), 0.0);
        loudness.set_sensitivity(100.0);
        assert_eq!(run(&mut loudness, 0.9, 0.1), MAX_ENERGY);
        assert_eq!(loudness.gain(), REFERENCE_GAIN);
    }

    #[test]
    fn empty_chunks_and_bad_numbers_decay_without_nan() {
        let mut loudness = Loudness::new(0.02, 0.25, true, 1.0, 0.02);
        run(&mut loudness, 0.2, 2.0);
        for _ in 0..600 {
            let energy = loudness.process(0.0, 0, DT);
            assert!(energy.is_finite());
        }
        assert_eq!(loudness.process(f64::NAN, 10, DT), 0.0);
        assert!(loudness.process(1.0, 10, f32::NAN).is_finite());
    }
}
