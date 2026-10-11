//! A deterministic built-in synth that needs no audio system.
//!
//! It plays phrases of six notes (0.8 s each, four harmonics) climbing in
//! fourths through three octaves, so that in time every pitch class sounds in
//! every octave. A kick drum hits on each beat (0.4 s), and each phrase ends
//! with 0.6 s of silence. The seed only chooses the starting note; the sound
//! is a pure function of time, so a given seed always renders identically.

use super::{AudioError, SampleSource};

pub const SAMPLE_RATE: u32 = 48_000;

const BEAT: f64 = 0.4;
const NOTE: f64 = 0.8;
const NOTES_PER_PHRASE: u64 = 6;
const REST: f64 = 0.6;
const PHRASE: f64 = NOTE * NOTES_PER_PHRASE as f64 + REST;
/// The melody spans C3 (MIDI 48) up to B5 and climbs by a perfect fourth.
const LOWEST_MIDI: i64 = 48;
const SPAN: i64 = 36;
const STEP: i64 = 5;
/// Relative amplitudes of the harmonics of a note.
const HARMONICS: [f64; 4] = [1.0, 0.5, 0.33, 0.2];
const NOTE_LEVEL: f64 = 0.13;
const KICK_LEVEL: f64 = 0.4;
const CLICK_LEVEL: f64 = 0.22;

#[derive(Debug, Clone)]
pub struct DemoSynth {
    start: i64,
    seed: u64,
    /// Index of the next sample to render.
    position: u64,
    carry: f64,
}

impl DemoSynth {
    pub fn new(seed: u64) -> Self {
        Self {
            start: fastrand::Rng::with_seed(seed).i64(0..SPAN),
            seed,
            position: 0,
            carry: 0.0,
        }
    }

    /// MIDI note number of the `index`-th note of the melody.
    pub fn midi_of(&self, index: u64) -> i64 {
        LOWEST_MIDI + (self.start + STEP * (index % SPAN as u64) as i64).rem_euclid(SPAN)
    }

    /// The note sounding at time `t` (seconds), as `(index, seconds into it)`.
    pub fn note_at(t: f64) -> Option<(u64, f64)> {
        let phrase = (t / PHRASE).floor();
        let within = t - phrase * PHRASE;
        if within >= NOTE * NOTES_PER_PHRASE as f64 {
            return None;
        }
        let slot = (within / NOTE).floor();
        Some((
            phrase as u64 * NOTES_PER_PHRASE + slot as u64,
            within - slot * NOTE,
        ))
    }

    /// One sample of the demo at an absolute sample index.
    pub fn sample(&self, index: u64) -> f32 {
        let t = index as f64 / f64::from(SAMPLE_RATE);
        let Some((note, age)) = Self::note_at(t) else {
            return 0.0;
        };
        let frequency = 440.0 * ((self.midi_of(note) - 69) as f64 / 12.0).exp2();
        let attack = (age / 0.008).min(1.0);
        let release = ((NOTE - age) / 0.04).min(1.0);
        let body = 0.55 + 0.45 * (-age / 0.4).exp();
        let mut tone = 0.0;
        for (harmonic, amplitude) in HARMONICS.iter().enumerate() {
            let hz = frequency * (harmonic + 1) as f64;
            // Keep partials safely below Nyquist whatever the melody.
            if hz < f64::from(SAMPLE_RATE) / 2.2 {
                tone += amplitude * (std::f64::consts::TAU * hz * age).sin();
            }
        }
        let tone = NOTE_LEVEL * tone * attack * release * body;

        // Kick: a low sine sweeping down, plus a short broadband click.
        let kick_age = age_in_beat(t);
        let sweep = 45.0 * kick_age + 55.0 * 0.03 * (1.0 - (-kick_age / 0.03).exp());
        let thump = KICK_LEVEL
            * (kick_age / 0.002).min(1.0)
            * (-kick_age / 0.06).exp()
            * (std::f64::consts::TAU * sweep).sin();
        let click = CLICK_LEVEL * (-kick_age / 0.004).exp() * noise(index, self.seed);
        ((tone + thump + click) as f32).clamp(-1.0, 1.0)
    }

    /// Render the next `out.len()` samples.
    pub fn generate(&mut self, out: &mut [f32]) {
        for sample in out {
            *sample = self.sample(self.position);
            self.position += 1;
        }
    }
}

/// Seconds since the last beat, counted from the start of the phrase.
fn age_in_beat(t: f64) -> f64 {
    let within = t - (t / PHRASE).floor() * PHRASE;
    within - (within / BEAT).floor() * BEAT
}

/// Deterministic white noise in -1..1 from a counter (splitmix64 finalizer).
fn noise(index: u64, seed: u64) -> f64 {
    let mut value = index
        .wrapping_add(seed)
        .wrapping_add(0x9E37_79B9_7F4A_7C15)
        .wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 30;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 27;
    (value >> 11) as f64 / (1_u64 << 52) as f64 - 1.0
}

impl SampleSource for DemoSynth {
    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn pull(&mut self, dt: f32, out: &mut Vec<f32>) -> Result<(), AudioError> {
        if dt.is_finite() && dt > 0.0 {
            self.carry += f64::from(dt) * f64::from(SAMPLE_RATE);
        }
        let count = (self.carry + 1e-4).floor() as usize;
        self.carry -= count as f64;
        let start = out.len();
        out.resize(start + count, 0.0);
        self.generate(&mut out[start..]);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn render(synth: &mut DemoSynth, seconds: f64) -> Vec<f32> {
        let mut out = vec![0.0; (seconds * f64::from(SAMPLE_RATE)) as usize];
        synth.generate(&mut out);
        out
    }

    #[test]
    fn the_same_seed_renders_identically_and_seeds_differ() {
        let first = render(&mut DemoSynth::new(42), 1.0);
        assert_eq!(first, render(&mut DemoSynth::new(42), 1.0));
        assert_ne!(first, render(&mut DemoSynth::new(43), 1.0));
        // Pulling in odd-sized steps yields the very same stream.
        let mut stepped = DemoSynth::new(42);
        let mut out = Vec::new();
        for _ in 0..60 {
            stepped.pull(1.0 / 60.0, &mut out).unwrap();
        }
        assert!((out.len() as i64 - 48_000).abs() <= 1);
        assert_eq!(out, first[..out.len()]);
    }

    #[test]
    fn the_melody_visits_every_pitch_class_in_every_octave() {
        let synth = DemoSynth::new(7);
        let mut seen = std::collections::BTreeSet::new();
        for index in 0..36 {
            let midi = synth.midi_of(index);
            assert!((48..84).contains(&midi), "{midi}");
            seen.insert(midi);
        }
        assert_eq!(seen.len(), 36);
        assert_eq!(
            seen.iter()
                .map(|midi| midi % 12)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            12
        );
        // The cycle repeats after 36 notes.
        assert_eq!(synth.midi_of(5), synth.midi_of(41));
        // Consecutive notes climb a fourth, wrapping down by three octaves.
        for index in 0..35 {
            let step = (synth.midi_of(index + 1) - synth.midi_of(index)).rem_euclid(12);
            assert_eq!(step, 5);
        }
    }

    #[test]
    fn timeline_has_six_notes_then_a_silent_rest() {
        assert_eq!(DemoSynth::note_at(0.0), Some((0, 0.0)));
        let (index, age) = DemoSynth::note_at(0.85).unwrap();
        assert_eq!(index, 1);
        assert!((age - 0.05).abs() < 1e-9);
        assert_eq!(DemoSynth::note_at(4.79).unwrap().0, 5);
        assert_eq!(DemoSynth::note_at(4.81), None);
        assert_eq!(DemoSynth::note_at(5.39), None);
        assert_eq!(DemoSynth::note_at(5.41).unwrap().0, 6);
        let mut synth = DemoSynth::new(1);
        let quiet = render(&mut synth, 5.4)[(4.9 * 48_000.0) as usize..].to_vec();
        assert!(quiet.iter().all(|&sample| sample == 0.0));
    }

    #[test]
    fn the_signal_is_bounded_and_has_a_kick_on_every_beat() {
        let samples = render(&mut DemoSynth::new(3), 4.8);
        assert!(samples.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        let window = |start: f64, length: f64| {
            let range = (start * 48_000.0) as usize..((start + length) * 48_000.0) as usize;
            (samples[range.clone()].iter().map(|s| s * s).sum::<f32>() / range.len() as f32).sqrt()
        };
        // The 30 ms after each beat is louder than the 30 ms before the next.
        for beat in 1..12 {
            let at = f64::from(beat) * 0.4;
            assert!(
                window(at, 0.03) > 1.3 * window(at - 0.05, 0.03),
                "beat {beat}"
            );
        }
    }
}
