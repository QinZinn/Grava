//! Musical mapping from audio features to weighted vibration modes.
//!
//! A [`Mapper`] turns [`AudioFeatures`] into a target set of modes, switching
//! only after a candidate has kept winning for `hold_ms`, and slews the active
//! weights towards that target over `transition_ms`. The catalog order never
//! depends on terminal size; size only decides which entries are reachable.

use crate::{
    config::schema::{Config, MappingMode, MaxOrder, Shape, Symmetry},
    dsp::AudioFeatures,
};

use super::{CircleMode, Mode, ModeCatalog, SquareMode, order_limit};

/// The active field combines at most this many modes.
pub const MAX_MODES: usize = 8;

/// Weights below this are considered gone.
const NEGLIGIBLE_WEIGHT: f32 = 1e-4;
/// Spectrum-mode weights follow peak magnitude to this power.
const PEAK_WEIGHT_EXPONENT: f32 = 1.5;
/// Time constant of the smoothing of spectrum weights, in milliseconds.
const SPECTRUM_SMOOTHING_MS: f32 = 100.0;
/// With `octave_complexity` off, every note uses this tier.
const FIXED_TIER: usize = 2;
/// Pitch-class names as written in `mapping.notes`.
pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// A filler for unused slots of fixed-capacity arrays.
const FILLER: Mode = Mode::Circle(CircleMode::FUNDAMENTAL);

/// Up to eight modes with nonnegative weights. It holds both the committed
/// target (weights sum to 1) and the slewed weights that drive the field.
#[derive(Clone, Copy, Debug)]
pub struct ModeWeights {
    entries: [(Mode, f32); MAX_MODES],
    len: usize,
}

/// Unused slots hold filler, so only the live entries are compared.
impl PartialEq for ModeWeights {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl ModeWeights {
    pub const fn empty() -> Self {
        Self {
            entries: [(FILLER, 0.0); MAX_MODES],
            len: 0,
        }
    }

    pub fn single(mode: Mode) -> Self {
        let mut weights = Self::empty();
        weights.entries[0] = (mode, 1.0);
        weights.len = 1;
        weights
    }

    pub fn as_slice(&self) -> &[(Mode, f32)] {
        &self.entries[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn weight_of(&self, mode: Mode) -> f32 {
        self.as_slice()
            .iter()
            .find(|entry| entry.0 == mode)
            .map_or(0.0, |entry| entry.1)
    }

    /// Whether both sets name the same modes, ignoring order and weights.
    pub fn same_modes(&self, other: &Self) -> bool {
        self.len == other.len
            && self
                .as_slice()
                .iter()
                .all(|&(mode, _)| other.as_slice().iter().any(|entry| entry.0 == mode))
    }

    /// Add `weight` to `mode`, inserting it if there is room.
    fn accumulate(&mut self, mode: Mode, weight: f32) {
        if let Some(entry) = self.entries[..self.len].iter_mut().find(|e| e.0 == mode) {
            entry.1 += weight;
        } else if self.len < MAX_MODES {
            self.entries[self.len] = (mode, weight);
            self.len += 1;
        }
    }

    fn normalize(&mut self) {
        let total: f32 = self.as_slice().iter().map(|entry| entry.1).sum();
        if total > 0.0 {
            for entry in &mut self.entries[..self.len] {
                entry.1 /= total;
            }
        }
    }

    /// Move every weight towards `target` by at most `step`. Modes missing
    /// from the target fade out and are dropped once negligible; target modes
    /// not yet present fade in. A step of 1 or more is a hard cut.
    fn slew_toward(&mut self, target: &Self, step: f32) {
        for entry in &mut self.entries[..self.len] {
            let desired = target.weight_of(entry.0);
            entry.1 = if entry.1 < desired {
                (entry.1 + step).min(desired)
            } else {
                (entry.1 - step).max(desired)
            };
        }
        for &(mode, desired) in target.as_slice() {
            if self.as_slice().iter().any(|entry| entry.0 == mode) {
                continue;
            }
            let slot = if self.len < MAX_MODES {
                self.len += 1;
                self.len - 1
            } else {
                // Full: only a mode that is leaving can make room. Targets
                // never exceed the capacity, so one always exists.
                let mut weakest = None;
                for (index, entry) in self.as_slice().iter().enumerate() {
                    if target.weight_of(entry.0) == 0.0
                        && weakest.is_none_or(|(_, weight)| entry.1 < weight)
                    {
                        weakest = Some((index, entry.1));
                    }
                }
                match weakest {
                    Some((index, _)) => index,
                    None => continue,
                }
            };
            self.entries[slot] = (mode, desired.min(step));
        }
        let mut kept = 0;
        for index in 0..self.len {
            let entry = self.entries[index];
            if entry.1 > NEGLIGIBLE_WEIGHT || target.weight_of(entry.0) > 0.0 {
                self.entries[kept] = entry;
                kept += 1;
            }
        }
        self.len = kept;
    }
}

struct Pending {
    candidate: ModeWeights,
    elapsed_ms: f32,
}

/// Maps features to modes for one plate shape and size.
pub struct Mapper {
    kind: MappingMode,
    octave_complexity: bool,
    base_octave: i32,
    min_confidence: f32,
    hold_ms: f32,
    transition_ms: f32,
    peaks: usize,
    low_hz: f32,
    high_hz: f32,
    catalog: ModeCatalog,
    pins: [Option<Mode>; 12],
    limit: u8,
    target: ModeWeights,
    /// Catalog index of the committed single-mode target in frequency mode.
    target_index: Option<usize>,
    pending: Option<Pending>,
    weights: ModeWeights,
    /// Tiers added to the mapped figure by the arrow keys.
    tier_shift: i32,
}

impl Mapper {
    /// `plate_width` and `plate_height` are in sub-pixels.
    pub fn new(config: &Config, plate_width: usize, plate_height: usize) -> Self {
        let catalog = ModeCatalog::for_plate(&config.plate);
        let mut pins = [None; 12];
        for (name, &[first, second]) in &config.mapping.notes {
            if let Some(class) = NOTE_NAMES.iter().position(|note| note == name) {
                pins[class] = pinned_mode(config, first, second);
            }
        }
        let mapping = &config.mapping;
        Self {
            kind: mapping.mode,
            octave_complexity: mapping.octave_complexity,
            base_octave: i32::from(mapping.base_octave),
            min_confidence: mapping.min_confidence,
            hold_ms: mapping.hold_ms as f32,
            transition_ms: mapping.transition_ms as f32,
            peaks: mapping.peaks.clamp(1, MAX_MODES),
            low_hz: config.analysis.low_cutoff.max(1.0),
            high_hz: config.analysis.high_cutoff,
            limit: order_limit(config.plate.max_order, plate_width, plate_height),
            catalog,
            pins,
            target: ModeWeights::empty(),
            target_index: None,
            pending: None,
            weights: ModeWeights::empty(),
            tier_shift: 0,
        }
    }

    /// Take over the figure and the tier of `previous`, so that rebuilding the
    /// mapper (a configuration reload) does not blank the plate. Only a
    /// previous mapper with the very same catalog can hand over its modes.
    pub fn adopt(&mut self, previous: Self) {
        self.tier_shift = previous.tier_shift;
        if previous.catalog.modes() == self.catalog.modes() {
            self.target = previous.target;
            self.target_index = previous.target_index;
            self.weights = previous.weights;
        }
    }

    /// Move the figures one tier up (`delta` > 0, more intricate) or down.
    /// Returns the tier shift now in effect.
    pub fn shift_tier(&mut self, delta: i32) -> i32 {
        let tiers = (self.catalog.len() / 12).max(1) as i32;
        self.tier_shift = (self.tier_shift + delta).clamp(1 - tiers, tiers - 1);
        self.pending = None;
        self.tier_shift
    }

    pub fn tier_shift(&self) -> i32 {
        self.tier_shift
    }

    /// A catalog index moved by the tier shift, one tier being twelve entries.
    fn shifted(&self, index: usize) -> usize {
        let last = self.catalog.len().saturating_sub(1) as i32;
        (index as i32 + 12 * self.tier_shift).clamp(0, last) as usize
    }

    /// Recompute reachability for a new plate size. Catalog indices are
    /// untouched, so a note keeps its figure whenever it is still reachable.
    pub fn resize(&mut self, max_order: MaxOrder, plate_width: usize, plate_height: usize) {
        self.limit = order_limit(max_order, plate_width, plate_height);
    }

    pub fn catalog(&self) -> &ModeCatalog {
        &self.catalog
    }

    /// The slewed weights that drive the active field.
    pub fn weights(&self) -> &ModeWeights {
        &self.weights
    }

    /// The committed target the weights are heading for.
    pub fn target(&self) -> &ModeWeights {
        &self.target
    }

    /// Whether the figure has arrived: nothing is waiting to be held and the
    /// weights have reached the target, so silence cannot change them.
    pub fn is_settled(&self) -> bool {
        self.pending.is_none()
            && self.weights.len() == self.target.len()
            && self
                .target
                .as_slice()
                .iter()
                .all(|&(mode, weight)| self.weights.weight_of(mode) == weight)
    }

    /// The mode with the largest current weight, for the HUD.
    pub fn dominant_mode(&self) -> Option<Mode> {
        self.weights
            .as_slice()
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|entry| entry.0)
    }

    /// Advance by `dt` seconds. Silence and unreliable pitch leave the target
    /// as it is, so the plate keeps its figure.
    pub fn update(&mut self, features: &AudioFeatures, dt: f32) -> &ModeWeights {
        let dt_ms = if dt.is_finite() && dt > 0.0 {
            dt * 1000.0
        } else {
            0.0
        };
        let candidate = if features.energy > 0.0 {
            match self.kind {
                MappingMode::Note => self.note_candidate(features),
                MappingMode::Frequency => self.frequency_candidate(features),
                MappingMode::Spectrum => self.spectrum_candidate(features),
            }
        } else {
            None
        };
        self.offer(candidate, dt_ms);
        let step = if self.transition_ms > 0.0 {
            dt_ms / self.transition_ms
        } else {
            f32::MAX
        };
        self.weights.slew_toward(&self.target, step);
        &self.weights
    }

    fn reachable(&self, index: usize) -> bool {
        self.catalog
            .get(index)
            .is_some_and(|mode| mode.order() <= self.limit)
    }

    /// The highest reachable catalog index at or below `index`.
    fn reachable_at_or_below(&self, index: usize) -> usize {
        let top = index.min(self.catalog.len().saturating_sub(1));
        (0..=top).rev().find(|&i| self.reachable(i)).unwrap_or(0)
    }

    fn note_candidate(&self, features: &AudioFeatures) -> Option<ModeWeights> {
        let class = usize::from(features.pitch_class?);
        if features.confidence < self.min_confidence || class >= 12 {
            return None;
        }
        if let Some(mode) = self.pins[class] {
            return Some(ModeWeights::single(mode));
        }
        let tiers = (self.catalog.len() / 12).max(1);
        let tier = if self.octave_complexity {
            i32::from(features.octave) - self.base_octave
        } else {
            FIXED_TIER as i32
        };
        let tier = (tier + self.tier_shift).clamp(0, tiers as i32 - 1) as usize;
        // The same pitch class in the highest reachable tier at or below the
        // wanted one; resizing therefore never reshuffles reachable notes.
        let index = (0..=tier)
            .rev()
            .map(|t| t * 12 + class)
            .find(|&index| self.reachable(index))
            .unwrap_or_else(|| {
                // Not even tier 0 reaches this class on a tiny plate: reuse
                // the reachable entries in order.
                let count = (0..self.catalog.len())
                    .filter(|&i| self.reachable(i))
                    .count()
                    .max(1);
                (0..self.catalog.len())
                    .filter(|&i| self.reachable(i))
                    .nth(class % count)
                    .unwrap_or(0)
            });
        self.catalog.get(index).map(ModeWeights::single)
    }

    /// Catalog index of a frequency: log scale between the cutoffs, clamped
    /// to the highest reachable entry.
    fn frequency_index(&self, hz: f32) -> usize {
        let span = (self.high_hz / self.low_hz).ln();
        let position = if span > 0.0 && hz > 0.0 {
            ((hz / self.low_hz).ln() / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let wanted = (position * (self.catalog.len() - 1) as f32).round() as usize;
        self.reachable_at_or_below(self.shifted(wanted))
    }

    fn frequency_candidate(&self, features: &AudioFeatures) -> Option<ModeWeights> {
        features.pitch_class?;
        if features.confidence < self.min_confidence {
            return None;
        }
        let mut index = self.frequency_index(features.dominant_hz?);
        // Ignore wobble of a single catalog step (a bit over half a semitone).
        if let Some(current) = self.target_index
            && index.abs_diff(current) <= 1
            && self.reachable(current)
        {
            index = current;
        }
        self.catalog.get(index).map(ModeWeights::single)
    }

    fn spectrum_candidate(&self, features: &AudioFeatures) -> Option<ModeWeights> {
        let mut candidate = ModeWeights::empty();
        for &(hz, magnitude) in features.peak_slice().iter().take(self.peaks) {
            if !hz.is_finite() || !magnitude.is_finite() || magnitude <= 0.0 {
                continue;
            }
            if let Some(mode) = self.catalog.get(self.frequency_index(hz)) {
                candidate.accumulate(mode, magnitude.powf(PEAK_WEIGHT_EXPONENT));
            }
        }
        if candidate.is_empty() {
            return None;
        }
        candidate.normalize();
        Some(candidate)
    }

    /// Apply the stability rules: a candidate that differs from the target
    /// replaces it only after winning for `hold_ms` in a row.
    fn offer(&mut self, candidate: Option<ModeWeights>, dt_ms: f32) {
        let Some(candidate) = candidate else {
            self.pending = None;
            return;
        };
        if candidate.same_modes(&self.target) {
            self.pending = None;
            self.follow_spectrum(&candidate, dt_ms);
            return;
        }
        let elapsed_ms = match &mut self.pending {
            Some(pending) if pending.candidate.same_modes(&candidate) => {
                pending.elapsed_ms += dt_ms;
                pending.candidate = candidate;
                pending.elapsed_ms
            }
            _ => {
                self.pending = Some(Pending {
                    candidate,
                    elapsed_ms: 0.0,
                });
                0.0
            }
        };
        if elapsed_ms >= self.hold_ms {
            self.target = candidate;
            self.target_index = (candidate.len() == 1)
                .then(|| {
                    self.catalog
                        .modes()
                        .iter()
                        .position(|&mode| mode == candidate.entries[0].0)
                })
                .flatten();
            self.pending = None;
        }
    }

    /// In spectrum mode the weights of an unchanged set follow the peaks.
    fn follow_spectrum(&mut self, candidate: &ModeWeights, dt_ms: f32) {
        if self.kind != MappingMode::Spectrum {
            return;
        }
        let blend = 1.0 - (-dt_ms / SPECTRUM_SMOOTHING_MS).exp();
        for entry in &mut self.target.entries[..self.target.len] {
            entry.1 += (candidate.weight_of(entry.0) - entry.1) * blend;
        }
        self.target.normalize();
    }
}

/// A figure pinned to a note; `first` and `second` are validated by the config.
fn pinned_mode(config: &Config, first: u8, second: u8) -> Option<Mode> {
    match config.plate.shape {
        Shape::Circle => CircleMode::new(first, second).ok().map(Mode::Circle),
        Shape::Square => {
            let symmetry = match config.plate.symmetry {
                Symmetry::Mixed if first == second => Symmetry::Sym,
                Symmetry::Mixed => Symmetry::Anti,
                other => other,
            };
            SquareMode::new(first, second, config.plate.edge, symmetry)
                .ok()
                .map(Mode::Square)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::schema::Edge;

    const DT: f32 = 1.0 / 60.0;

    fn note(class: u8, octave: i8) -> AudioFeatures {
        AudioFeatures {
            energy: 0.6,
            pitch_class: Some(class),
            octave,
            confidence: 0.9,
            dominant_hz: Some(
                440.0 * 2.0_f32.powf((f32::from(class) - 9.0) / 12.0 + f32::from(octave - 4)),
            ),
            ..AudioFeatures::default()
        }
    }

    fn mapper(configure: impl FnOnce(&mut Config)) -> Mapper {
        let mut config = Config::default();
        configure(&mut config);
        Mapper::new(&config, 152, 152)
    }

    /// Feed one feature set for `seconds`, returning the committed target mode.
    fn feed(mapper: &mut Mapper, features: &AudioFeatures, seconds: f32) -> Option<Mode> {
        for _ in 0..(seconds / DT).round() as usize {
            mapper.update(features, DT);
        }
        mapper.target().as_slice().first().map(|entry| entry.0)
    }

    fn catalog_index(mapper: &Mapper, mode: Mode) -> usize {
        mapper
            .catalog()
            .modes()
            .iter()
            .position(|&m| m == mode)
            .unwrap()
    }

    #[test]
    fn a_mapper_is_settled_when_nothing_waits_and_the_weights_have_arrived() {
        let mut mapper = mapper(|config| config.mapping.transition_ms = 500);
        assert!(
            mapper.is_settled(),
            "a plate that has heard nothing is at rest"
        );
        let a4 = note(9, 4);
        // Hearing a note: a candidate is pending, then the weights are on their way.
        mapper.update(&a4, DT);
        assert!(!mapper.is_settled());
        feed(&mut mapper, &a4, 0.2);
        assert!(!mapper.is_settled(), "committed but still fading in");
        // Silence cannot change anything once the fade is over.
        feed(&mut mapper, &AudioFeatures::default(), 0.1);
        assert!(!mapper.is_settled());
        feed(&mut mapper, &AudioFeatures::default(), 1.0);
        assert!(mapper.is_settled());
        let before = *mapper.weights();
        feed(&mut mapper, &AudioFeatures::default(), 5.0);
        assert_eq!(*mapper.weights(), before);
        // A new note in the middle of a crossfade unsettles it again.
        feed(&mut mapper, &note(2, 4), 0.2);
        assert!(!mapper.is_settled());
        // A blip shorter than the hold time leaves a pending candidate, which
        // silence then discards.
        let mut mapper = self::mapper(|config| config.mapping.hold_ms = 500);
        feed(&mut mapper, &a4, 1.0);
        feed(&mut mapper, &note(2, 4), 0.1);
        assert!(!mapper.is_settled());
        mapper.update(&AudioFeatures::default(), DT);
        assert!(mapper.is_settled());
    }

    #[test]
    fn the_same_note_always_gets_the_same_figure() {
        let mut first = mapper(|_| {});
        let mut second = mapper(|_| {});
        let a4 = note(9, 4);
        let figure = feed(&mut first, &a4, 1.0).unwrap();
        feed(&mut second, &note(2, 3), 1.0);
        feed(&mut second, &note(5, 5), 1.0);
        assert_eq!(feed(&mut second, &a4, 1.0), Some(figure));
        let other = feed(&mut first, &note(10, 4), 1.0).unwrap();
        assert_ne!(other, figure);
    }

    #[test]
    fn note_mapping_indexes_the_catalog_by_tier_and_pitch_class() {
        let mut mapper = mapper(|_| {});
        for (class, octave) in [(0, 2), (9, 4), (11, 3), (4, 6), (7, 1)] {
            let mode = feed(&mut mapper, &note(class, octave), 1.0).unwrap();
            let tier = (i32::from(octave) - 2).clamp(0, 10) as usize;
            assert_eq!(catalog_index(&mapper, mode), tier * 12 + usize::from(class));
        }
        // Higher octaves choose more intricate figures; tiers saturate.
        let low = feed(&mut mapper, &note(3, 1), 1.0).unwrap();
        let mid = feed(&mut mapper, &note(3, 4), 1.0).unwrap();
        let high = feed(&mut mapper, &note(3, 9), 1.0).unwrap();
        let eigen = |mode: Mode| u16::from(mode.first()).pow(2) + u16::from(mode.second()).pow(2);
        assert!(eigen(low) < eigen(mid) && eigen(mid) < eigen(high));
        // Eleven tiers of twelve cover octaves 2..12; anything above saturates.
        let top = feed(&mut mapper, &note(3, 12), 1.0).unwrap();
        assert!(eigen(top) > eigen(high));
        assert_eq!(feed(&mut mapper, &note(3, 100), 1.0), Some(top));
    }

    #[test]
    fn octave_complexity_off_uses_one_tier() {
        let mut mapper = mapper(|config| config.mapping.octave_complexity = false);
        let low = feed(&mut mapper, &note(7, 1), 1.0).unwrap();
        let high = feed(&mut mapper, &note(7, 7), 1.0).unwrap();
        assert_eq!(low, high);
        assert_eq!(catalog_index(&mapper, low), FIXED_TIER * 12 + 7);
    }

    #[test]
    fn unreachable_entries_fall_back_to_a_lower_tier_of_the_same_class() {
        let reference = Mapper::new(&Config::default(), 300, 300);
        let small_reference = Mapper::new(&Config::default(), 24, 24);
        // The catalog itself is identical at every size.
        assert_eq!(
            reference.catalog().modes(),
            small_reference.catalog().modes()
        );
        for class in 0..12 {
            let wanted = note(class, 8);
            let mut big = Mapper::new(&Config::default(), 300, 300);
            let mut small = Mapper::new(&Config::default(), 24, 24);
            let big_mode = feed(&mut big, &wanted, 1.0).unwrap();
            let big_index = catalog_index(&big, big_mode);
            let small_mode = feed(&mut small, &wanted, 1.0).unwrap();
            assert!(small_mode.order() <= 4, "{small_mode}");
            let small_index = catalog_index(&small, small_mode);
            // The same pitch class whenever tier 0 reaches it at this size.
            if small.reachable(usize::from(class)) {
                assert_eq!(small_index % 12, usize::from(class));
            }
            assert!(small_index <= big_index);
        }
    }

    #[test]
    fn a_note_keeps_its_figure_across_resizes_while_it_stays_reachable() {
        let mut mapper = Mapper::new(&Config::default(), 152, 152);
        let a3 = note(9, 3);
        let before = feed(&mut mapper, &a3, 1.0).unwrap();
        mapper.resize(MaxOrder::Auto, 300, 300);
        let after = feed(&mut mapper, &a3, 1.0).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn a_brief_blip_does_not_switch_the_figure() {
        let mut mapper = mapper(|_| {});
        let figure = feed(&mut mapper, &note(9, 4), 1.0).unwrap();
        let weights = *mapper.weights();
        // 100 ms of another note is shorter than the default 120 ms hold.
        let blip = feed(&mut mapper, &note(3, 4), 0.1).unwrap();
        assert_eq!(blip, figure);
        assert_eq!(*mapper.weights(), weights);
        // Going back to A resets the pending candidate entirely.
        feed(&mut mapper, &note(9, 4), 0.05);
        feed(&mut mapper, &note(3, 4), 0.1);
        assert_eq!(mapper.target().as_slice()[0].0, figure);
        // A candidate that is held long enough does win.
        let switched = feed(&mut mapper, &note(3, 4), 0.2).unwrap();
        assert_ne!(switched, figure);
    }

    #[test]
    fn hold_time_is_respected_to_the_frame() {
        let mut mapper = mapper(|config| config.mapping.hold_ms = 200);
        feed(&mut mapper, &note(0, 3), 1.0);
        let target = *mapper.target();
        let candidate = note(7, 3);
        let mut frames = 0;
        while *mapper.target() == target {
            mapper.update(&candidate, DT);
            frames += 1;
            assert!(frames < 60);
        }
        // 200 ms at 60 fps is 12 frames after the first sighting.
        assert!((12..=14).contains(&frames), "{frames}");
        let mut instant = self::mapper(|config| config.mapping.hold_ms = 0);
        feed(&mut instant, &note(0, 3), 0.1);
        let first = *instant.target();
        instant.update(&note(7, 3), DT);
        assert_ne!(*instant.target(), first);
    }

    #[test]
    fn unreliable_pitch_and_silence_keep_the_current_figure() {
        let mut mapper = mapper(|_| {});
        let figure = feed(&mut mapper, &note(9, 4), 1.0).unwrap();
        let weights = *mapper.weights();
        let mut unsure = note(2, 4);
        unsure.confidence = 0.2;
        assert_eq!(feed(&mut mapper, &unsure, 2.0), Some(figure));
        let mut silent = note(2, 4);
        silent.energy = 0.0;
        silent.pitch_class = None;
        assert_eq!(feed(&mut mapper, &silent, 10.0), Some(figure));
        assert_eq!(*mapper.weights(), weights);
        // No pitch at all (percussion) behaves like uncertainty.
        let mut noise = note(2, 4);
        noise.pitch_class = None;
        assert_eq!(feed(&mut mapper, &noise, 1.0), Some(figure));
        // A blip followed by silence must not linger as a pending candidate.
        feed(&mut mapper, &note(5, 4), 0.1);
        feed(&mut mapper, &silent, 1.0);
        feed(&mut mapper, &note(5, 4), 0.1);
        assert_eq!(mapper.target().as_slice()[0].0, figure);
    }

    #[test]
    fn crossfade_is_linear_continuous_and_interruptible() {
        let mut mapper = mapper(|config| {
            config.mapping.hold_ms = 0;
            config.mapping.transition_ms = 350;
        });
        let first = feed(&mut mapper, &note(0, 3), 1.0).unwrap();
        assert_eq!(mapper.weights().weight_of(first), 1.0);
        let second_features = note(7, 3);
        let mut previous = *mapper.weights();
        let mut frames = 0;
        let second = loop {
            mapper.update(&second_features, DT);
            frames += 1;
            let weights = *mapper.weights();
            // Per-frame change is bounded by dt / transition and the total is
            // conserved while one figure replaces another.
            let total: f32 = weights.as_slice().iter().map(|e| e.1).sum();
            assert!((total - 1.0).abs() < 1e-4, "{total}");
            for &(mode, weight) in weights.as_slice() {
                let before = previous.weight_of(mode);
                assert!((weight - before).abs() <= DT / 0.35 + 1e-5);
            }
            previous = weights;
            if weights.len() == 1 {
                break weights.as_slice()[0].0;
            }
            assert!(frames < 60);
        };
        assert_ne!(second, first);
        assert!((20..=23).contains(&frames), "{frames}");
        // Interrupt halfway: the field must not jump.
        mapper.update(&note(2, 3), DT);
        for _ in 0..10 {
            mapper.update(&note(2, 3), DT);
        }
        let halfway = *mapper.weights();
        mapper.update(&note(4, 3), DT);
        for &(mode, weight) in mapper.weights().as_slice() {
            assert!((weight - halfway.weight_of(mode)).abs() <= DT / 0.35 + 1e-5);
        }
    }

    #[test]
    fn zero_transition_cuts_immediately() {
        let mut mapper = mapper(|config| {
            config.mapping.hold_ms = 0;
            config.mapping.transition_ms = 0;
        });
        feed(&mut mapper, &note(0, 3), 0.1);
        let figure = feed(&mut mapper, &note(5, 3), 0.1).unwrap();
        assert_eq!(*mapper.weights(), ModeWeights::single(figure));
    }

    #[test]
    fn frequency_mapping_is_monotonic_log_scaled_and_clamped() {
        let mut mapper = mapper(|config| config.mapping.mode = MappingMode::Frequency);
        let mut previous = 0;
        for hz in [60.0_f32, 100.0, 200.0, 400.0, 800.0, 1600.0] {
            let features = AudioFeatures {
                energy: 0.5,
                pitch_class: Some(0),
                confidence: 0.9,
                dominant_hz: Some(hz),
                ..AudioFeatures::default()
            };
            let mode = feed(&mut mapper, &features, 0.5).unwrap();
            let index = catalog_index(&mapper, mode);
            assert!(index > previous, "{hz} Hz -> {index} after {previous}");
            previous = index;
        }
        // Octaves are equally spaced on a log scale.
        let spacing: Vec<_> = {
            let mut indices = Vec::new();
            for hz in [100.0_f32, 200.0, 400.0, 800.0] {
                let features = AudioFeatures {
                    energy: 0.5,
                    pitch_class: Some(0),
                    confidence: 0.9,
                    dominant_hz: Some(hz),
                    ..AudioFeatures::default()
                };
                let mode = feed(&mut mapper, &features, 0.5).unwrap();
                indices.push(catalog_index(&mapper, mode) as i32);
            }
            indices.windows(2).map(|pair| pair[1] - pair[0]).collect()
        };
        assert!(
            spacing.iter().all(|&step| (step - spacing[0]).abs() <= 2),
            "{spacing:?}"
        );
        // A tiny plate clamps high frequencies to its highest reachable mode.
        let mut tiny = Mapper::new(&small_frequency_config(), 24, 24);
        let top = AudioFeatures {
            energy: 0.5,
            pitch_class: Some(0),
            confidence: 0.9,
            dominant_hz: Some(7000.0),
            ..AudioFeatures::default()
        };
        let mode = feed(&mut tiny, &top, 0.5).unwrap();
        assert!(mode.order() <= 4);
        let index = catalog_index(&tiny, mode);
        assert!((index + 1..tiny.catalog().len()).all(|i| !tiny.reachable(i)));
    }

    fn small_frequency_config() -> Config {
        let mut config = Config::default();
        config.mapping.mode = MappingMode::Frequency;
        config
    }

    #[test]
    fn frequency_mapping_ignores_single_step_wobble() {
        let mut mapper = mapper(|config| {
            config.mapping.mode = MappingMode::Frequency;
            config.mapping.hold_ms = 0;
        });
        let at = |hz: f32| AudioFeatures {
            energy: 0.5,
            pitch_class: Some(0),
            confidence: 0.9,
            dominant_hz: Some(hz),
            ..AudioFeatures::default()
        };
        let settled = feed(&mut mapper, &at(300.0), 0.5).unwrap();
        for hz in [296.0, 304.0, 298.0, 303.0] {
            assert_eq!(feed(&mut mapper, &at(hz), 0.2), Some(settled), "{hz} Hz");
        }
        assert_ne!(feed(&mut mapper, &at(360.0), 0.2), Some(settled));
    }

    #[test]
    fn spectrum_mapping_combines_peaks_with_magnitude_weights() {
        let mut mapper = mapper(|config| {
            config.mapping.mode = MappingMode::Spectrum;
            config.mapping.peaks = 3;
            config.mapping.hold_ms = 0;
        });
        let mut features = AudioFeatures {
            energy: 0.5,
            ..AudioFeatures::default()
        };
        features.peaks[0] = (110.0, 0.4);
        features.peaks[1] = (440.0, 0.2);
        features.peaks[2] = (1760.0, 0.1);
        features.peaks[3] = (3000.0, 0.09);
        features.peak_count = 4;
        for _ in 0..120 {
            mapper.update(&features, DT);
        }
        let weights = *mapper.weights();
        // Only the top three peaks count, each at its own mode.
        assert_eq!(weights.len(), 3);
        let total: f32 = weights.as_slice().iter().map(|e| e.1).sum();
        assert!((total - 1.0).abs() < 1e-3);
        let raw = [0.4_f32.powf(1.5), 0.2_f32.powf(1.5), 0.1_f32.powf(1.5)];
        let sum: f32 = raw.iter().sum();
        let mut sorted: Vec<f32> = weights.as_slice().iter().map(|e| e.1).collect();
        sorted.sort_by(|a, b| b.total_cmp(a));
        for (actual, expected) in sorted.iter().zip(raw.map(|r| r / sum)) {
            assert!((actual - expected).abs() < 0.02, "{sorted:?}");
        }
        // Without peaks (or energy) the figure stays.
        let kept = *mapper.weights();
        mapper.update(&AudioFeatures::default(), DT);
        assert_eq!(*mapper.weights(), kept);
    }

    #[test]
    fn spectrum_mapping_merges_peaks_sharing_a_mode_and_holds_the_set() {
        let mut mapper = mapper(|config| {
            config.mapping.mode = MappingMode::Spectrum;
            config.mapping.peaks = 8;
        });
        let mut features = AudioFeatures {
            energy: 0.5,
            ..AudioFeatures::default()
        };
        features.peaks[0] = (440.0, 0.3);
        features.peaks[1] = (441.0, 0.3);
        features.peak_count = 2;
        for _ in 0..60 {
            mapper.update(&features, DT);
        }
        assert_eq!(mapper.weights().len(), 1);
        // A one-frame extra peak does not enter the target set.
        let mut blip = features;
        blip.peaks[2] = (2000.0, 0.25);
        blip.peak_count = 3;
        mapper.update(&blip, DT);
        for _ in 0..60 {
            mapper.update(&features, DT);
        }
        assert_eq!(mapper.weights().len(), 1);
    }

    #[test]
    fn pinned_notes_override_the_catalog_for_every_octave() {
        let mut mapper = mapper(|config| {
            config.mapping.notes.insert("A".to_owned(), [2, 5]);
            config.mapping.notes.insert("C#".to_owned(), [0, 3]);
        });
        for octave in [1, 4, 8] {
            let mode = feed(&mut mapper, &note(9, octave), 0.5).unwrap();
            assert_eq!((mode.first(), mode.second()), (2, 5));
            let mode = feed(&mut mapper, &note(1, octave), 0.5).unwrap();
            assert_eq!((mode.first(), mode.second()), (0, 3));
        }
        // Unpinned notes still use the catalog.
        let mode = feed(&mut mapper, &note(4, 2), 0.5).unwrap();
        assert_eq!(catalog_index(&mapper, mode), 4);
    }

    #[test]
    fn drum_plates_map_through_the_drum_catalog_and_pins() {
        let mut mapper = mapper(|config| {
            config.plate.shape = Shape::Circle;
            config.mapping.notes.insert("D".to_owned(), [3, 2]);
        });
        let figure = feed(&mut mapper, &note(0, 2), 0.5).unwrap();
        assert_eq!(figure.shape(), Shape::Circle);
        assert_eq!(figure.to_string(), "J(0,1)");
        let pinned = feed(&mut mapper, &note(2, 5), 0.5).unwrap();
        assert_eq!(pinned.to_string(), "J(3,2)");
        let intricate = feed(&mut mapper, &note(5, 6), 0.5).unwrap();
        assert!(intricate.order() <= 16);
        assert_eq!(catalog_index(&mapper, intricate) % 12, 5);
    }

    #[test]
    fn mode_weights_respect_capacity_and_drop_faded_modes() {
        let catalog = ModeCatalog::square(Edge::Free, Symmetry::Anti);
        let modes: Vec<Mode> = catalog.modes().iter().copied().take(12).collect();
        let mut weights = ModeWeights::empty();
        // Eight fading modes, then a new target arrives with the set full.
        let mut start = ModeWeights::empty();
        for &mode in &modes[..8] {
            start.accumulate(mode, 1.0);
        }
        start.normalize();
        weights.slew_toward(&start, 1.0);
        assert_eq!(weights.len(), 8);
        let target = ModeWeights::single(modes[9]);
        weights.slew_toward(&target, 0.1);
        assert_eq!(weights.len(), 8);
        assert!(weights.weight_of(modes[9]) > 0.0);
        for _ in 0..20 {
            weights.slew_toward(&target, 0.1);
        }
        assert_eq!(weights, target);
        // Accumulating beyond capacity ignores the extra mode.
        let mut full = ModeWeights::empty();
        for &mode in &modes[..9] {
            full.accumulate(mode, 1.0);
        }
        assert_eq!(full.len(), 8);
    }

    #[test]
    fn the_tier_shift_moves_note_figures_up_and_down_by_twelve_entries() {
        let mut mapper = mapper(|_| {});
        let base_mode = feed(&mut mapper, &note(9, 4), 1.0).unwrap();
        let base = catalog_index(&mapper, base_mode);
        assert_eq!(base, 2 * 12 + 9);
        assert_eq!(mapper.shift_tier(1), 1);
        let higher_mode = feed(&mut mapper, &note(9, 4), 1.0).unwrap();
        let higher = catalog_index(&mapper, higher_mode);
        assert_eq!(higher, 3 * 12 + 9);
        assert_eq!(mapper.shift_tier(-3), -2);
        let lower_mode = feed(&mut mapper, &note(9, 4), 1.0).unwrap();
        let lower = catalog_index(&mapper, lower_mode);
        assert_eq!(lower, 9);
        // The shift saturates at the ends of the catalog instead of running away.
        for _ in 0..40 {
            mapper.shift_tier(1);
        }
        assert_eq!(mapper.tier_shift(), 10);
        for _ in 0..40 {
            mapper.shift_tier(-1);
        }
        assert_eq!(mapper.tier_shift(), -10);
        // Frequency mode shifts by whole tiers as well.
        let mut frequency = self::mapper(|config| config.mapping.mode = MappingMode::Frequency);
        let features = AudioFeatures {
            energy: 0.5,
            pitch_class: Some(0),
            confidence: 0.9,
            dominant_hz: Some(300.0),
            ..AudioFeatures::default()
        };
        let before_mode = feed(&mut frequency, &features, 0.5).unwrap();
        let before = catalog_index(&frequency, before_mode);
        frequency.shift_tier(1);
        let after_mode = feed(&mut frequency, &features, 0.5).unwrap();
        let after = catalog_index(&frequency, after_mode);
        assert!(
            after > before && after - before <= 12 + 1,
            "{before} -> {after}"
        );
    }

    #[test]
    fn adopting_a_previous_mapper_keeps_the_figure_only_for_the_same_catalog() {
        let mut previous = mapper(|_| {});
        feed(&mut previous, &note(9, 4), 1.0);
        previous.shift_tier(2);
        let figure = *previous.weights();
        let mut same = Mapper::new(&Config::default(), 152, 152);
        same.adopt(previous);
        assert_eq!(*same.weights(), figure);
        assert_eq!(same.tier_shift(), 2);
        // A different plate shape cannot reuse square figures but keeps the tier.
        let mut previous = mapper(|_| {});
        feed(&mut previous, &note(9, 4), 1.0);
        previous.shift_tier(1);
        let mut drum_config = Config::default();
        drum_config.plate.shape = Shape::Circle;
        let mut drum = Mapper::new(&drum_config, 152, 152);
        drum.adopt(previous);
        assert!(drum.weights().is_empty() && drum.target().is_empty());
        assert_eq!(drum.tier_shift(), 1);
    }
}
