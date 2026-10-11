//! Normalized vibration grids, bilinear sampling, and a lazy LRU cache.

use super::{CircleMode, Mode, SquareMode};

const MIN_GRID_SIDE: usize = 256;
const DEFAULT_CACHE_CAPACITY: usize = 64;
const MAX_ACTIVE_MODES: usize = 8;
/// Entries in the radial lookup table of one drum mode.
const RADIAL_SAMPLES: usize = 2048;
/// The unit square's corner, in disc radii from its centre.
const MAX_RADIUS: f32 = std::f32::consts::SQRT_2;

/// Keep one interpolation corner together so a particle touches fewer cache lines.
#[derive(Debug, Clone, Copy, Default)]
struct FieldSample {
    amplitude: f32,
    gradient_x: f32,
    gradient_y: f32,
}

/// A view rotation about the plate centre, applied when sampling.
#[derive(Debug, Clone, Copy)]
struct Rotation {
    cos: f32,
    sin: f32,
}

impl Rotation {
    const IDENTITY: Self = Self { cos: 1.0, sin: 0.0 };

    fn is_identity(self) -> bool {
        self.sin == 0.0 && self.cos == 1.0
    }
}

/// A normalized amplitude grid with central-difference gradients in plate units.
///
/// A grid may be turned about the plate centre with [`Self::set_rotation`]. The
/// stored amplitudes stay unrotated; sampling turns the query point instead, so
/// a drifting drum head costs no rebuild.
#[derive(Debug, Clone)]
pub struct FieldGrid {
    side: usize,
    amplitudes: Vec<f32>,
    samples: Vec<FieldSample>,
    rotation: Rotation,
}

impl FieldGrid {
    /// Allocate an initially silent field; sampling and rebuilding reuse storage.
    pub fn for_plate(plate_width: usize, plate_height: usize) -> Self {
        Self::with_side(plate_width.max(plate_height).max(MIN_GRID_SIDE))
    }

    pub fn from_mode(mode: impl Into<Mode>, plate_width: usize, plate_height: usize) -> Self {
        let mut grid = Self::for_plate(plate_width, plate_height);
        let mut rasterizer = Rasterizer::new(grid.side);
        rasterizer.rasterize(mode.into(), &mut grid.amplitudes);
        grid.rebuild_gradients();
        grid
    }

    fn with_side(side: usize) -> Self {
        Self {
            side,
            amplitudes: vec![0.0; side * side],
            samples: vec![FieldSample::default(); side * side],
            rotation: Rotation::IDENTITY,
        }
    }

    pub fn side(&self) -> usize {
        self.side
    }

    /// Unrotated amplitudes in row-major order.
    pub fn amplitudes(&self) -> &[f32] {
        &self.amplitudes
    }

    /// Turn the figure by `angle` radians about the plate centre.
    pub fn set_rotation(&mut self, angle: f32) {
        self.rotation = if angle.is_finite() {
            let (sin, cos) = angle.sin_cos();
            Rotation { cos, sin }
        } else {
            Rotation::IDENTITY
        };
    }

    /// Bilinear interpolation of normalized |f|, with coordinates clamped to the plate.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        self.sample_with_gradient(x, y).0
    }

    /// Interpolate the amplitude and the precomputed central-difference gradient.
    /// This path performs no trigonometry or allocation per particle.
    #[inline]
    pub fn sample_with_gradient(&self, x: f32, y: f32) -> (f32, f32, f32) {
        let x = if x.is_finite() {
            x.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let y = if y.is_finite() {
            y.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.sample_in_plate_with_gradient(x, y)
    }

    /// Sample private simulation positions, which are finite and in `0..=1`.
    /// Callers must maintain this invariant; the public sampler also accepts
    /// arbitrary coordinates. The endpoint still selects the last grid cell.
    #[inline]
    pub(crate) fn sample_in_plate_with_gradient(&self, x: f32, y: f32) -> (f32, f32, f32) {
        debug_assert!(x.is_finite() && (0.0..=1.0).contains(&x));
        debug_assert!(y.is_finite() && (0.0..=1.0).contains(&y));
        if self.rotation.is_identity() {
            let (index, tx, ty) = self.cell_in_plate(x, y);
            return self.interpolate_with_gradient(index, tx, ty);
        }
        // f(p) = base(c + Q (p - c)) with Q = R(-angle); the gradient turns
        // back by Q^T. Rounding may push a rim point a hair outside the unit
        // square, so clamp.
        let Rotation { cos, sin } = self.rotation;
        let (dx, dy) = (x - 0.5, y - 0.5);
        let px = (0.5 + cos * dx + sin * dy).clamp(0.0, 1.0);
        let py = (0.5 - sin * dx + cos * dy).clamp(0.0, 1.0);
        let (index, tx, ty) = self.cell_in_plate(px, py);
        let (amplitude, gx, gy) = self.interpolate_with_gradient(index, tx, ty);
        (amplitude, cos * gx - sin * gy, sin * gx + cos * gy)
    }

    #[inline]
    fn interpolate_with_gradient(&self, index: usize, tx: f32, ty: f32) -> (f32, f32, f32) {
        let top_left = self.samples[index];
        let top_right = self.samples[index + 1];
        let bottom_left = self.samples[index + self.side];
        let bottom_right = self.samples[index + self.side + 1];
        let interpolate = |a, b, c, d| {
            let top = a + (b - a) * tx;
            let bottom = c + (d - c) * tx;
            top + (bottom - top) * ty
        };
        (
            interpolate(
                top_left.amplitude,
                top_right.amplitude,
                bottom_left.amplitude,
                bottom_right.amplitude,
            ),
            interpolate(
                top_left.gradient_x,
                top_right.gradient_x,
                bottom_left.gradient_x,
                bottom_right.gradient_x,
            ),
            interpolate(
                top_left.gradient_y,
                top_right.gradient_y,
                bottom_left.gradient_y,
                bottom_right.gradient_y,
            ),
        )
    }

    #[inline]
    fn cell_in_plate(&self, x: f32, y: f32) -> (usize, f32, f32) {
        let scale = (self.side - 1) as f32;
        let px = x * scale;
        let py = y * scale;
        let column = (px as usize).min(self.side - 2);
        let row = (py as usize).min(self.side - 2);
        (
            row * self.side + column,
            px - column as f32,
            py - row as f32,
        )
    }

    fn rebuild_gradients(&mut self) {
        let scale = (self.side - 1) as f32;
        for row in 0..self.side {
            let above = row.saturating_sub(1);
            let below = (row + 1).min(self.side - 1);
            let scale_y = scale / (below - above) as f32;
            for column in 0..self.side {
                let left = column.saturating_sub(1);
                let right = (column + 1).min(self.side - 1);
                let index = row * self.side + column;
                let gradient_x = (self.amplitudes[row * self.side + right]
                    - self.amplitudes[row * self.side + left])
                    * (scale / (right - left) as f32);
                let gradient_y = (self.amplitudes[below * self.side + column]
                    - self.amplitudes[above * self.side + column])
                    * scale_y;
                self.samples[index] = FieldSample {
                    amplitude: self.amplitudes[index],
                    gradient_x,
                    gradient_y,
                };
            }
        }
    }
}

/// Distance and angle of a grid point from the plate centre; the radius is
/// measured in disc radii, so the drum's rim is at 1.
#[derive(Debug, Clone, Copy)]
struct Polar {
    radius: f32,
    angle: f32,
}

/// Scratch storage for turning one mode into a normalized grid. The cache
/// shares one instance across all modes.
#[derive(Debug)]
struct Rasterizer {
    side: usize,
    axis_n: Vec<f32>,
    axis_m: Vec<f32>,
    /// Built on the first drum mode; square-only sessions never pay for it.
    polar: Vec<Polar>,
    radial: Vec<f32>,
}

impl Rasterizer {
    fn new(side: usize) -> Self {
        Self {
            side,
            axis_n: vec![0.0; side],
            axis_m: vec![0.0; side],
            polar: Vec::new(),
            radial: Vec::new(),
        }
    }

    /// Fill `amplitudes` with |f| normalized so that its maximum is 1.
    fn rasterize(&mut self, mode: Mode, amplitudes: &mut [f32]) {
        debug_assert_eq!(amplitudes.len(), self.side * self.side);
        match mode {
            Mode::Square(mode) => self.square(mode, amplitudes),
            Mode::Circle(mode) => self.circle(mode, amplitudes),
        }
    }

    fn square(&mut self, mode: SquareMode, amplitudes: &mut [f32]) {
        let scale = (self.side - 1) as f32;
        for index in 0..self.side {
            let coordinate = index as f32 / scale;
            self.axis_n[index] = mode.axis(mode.n(), coordinate);
            self.axis_m[index] = mode.axis(mode.m(), coordinate);
        }
        let mut maximum = 0.0_f32;
        for row in 0..self.side {
            for column in 0..self.side {
                let amplitude = mode
                    .combine(
                        self.axis_n[column],
                        self.axis_m[column],
                        self.axis_n[row],
                        self.axis_m[row],
                    )
                    .abs();
                amplitudes[row * self.side + column] = amplitude;
                maximum = maximum.max(amplitude);
            }
        }
        normalize(amplitudes, maximum);
    }

    fn circle(&mut self, mode: CircleMode, amplitudes: &mut [f32]) {
        if self.polar.is_empty() {
            let scale = (self.side - 1) as f32;
            self.polar.reserve_exact(self.side * self.side);
            for row in 0..self.side {
                for column in 0..self.side {
                    let dx = column as f32 / scale - 0.5;
                    let dy = row as f32 / scale - 0.5;
                    self.polar.push(Polar {
                        radius: 2.0 * (dx * dx + dy * dy).sqrt(),
                        angle: dy.atan2(dx),
                    });
                }
            }
            self.radial = vec![0.0; RADIAL_SAMPLES];
        }
        for (index, value) in self.radial.iter_mut().enumerate() {
            *value = mode.radial(index as f32 / (RADIAL_SAMPLES - 1) as f32 * MAX_RADIUS);
        }
        let lookup_scale = (RADIAL_SAMPLES - 1) as f32 / MAX_RADIUS;
        let order = f32::from(mode.n());
        // Normalize over the disc only: the continuation beyond the rim keeps
        // the gradient smooth at the boundary but must not set the scale.
        let mut maximum = 0.0_f32;
        for (amplitude, point) in amplitudes.iter_mut().zip(&self.polar) {
            let position = (point.radius * lookup_scale).min((RADIAL_SAMPLES - 1) as f32);
            let index = (position as usize).min(RADIAL_SAMPLES - 2);
            let fraction = position - index as f32;
            let radial =
                self.radial[index] + (self.radial[index + 1] - self.radial[index]) * fraction;
            let value = if mode.n() == 0 {
                radial
            } else {
                radial * (order * point.angle).cos()
            };
            *amplitude = value.abs();
            if point.radius <= 1.0 {
                maximum = maximum.max(*amplitude);
            }
        }
        normalize(amplitudes, maximum);
    }
}

fn normalize(amplitudes: &mut [f32], maximum: f32) {
    if maximum > 0.0 {
        for amplitude in amplitudes {
            *amplitude = (*amplitude / maximum).min(1.0);
        }
    }
}

#[derive(Debug)]
struct CacheEntry {
    mode: Mode,
    amplitudes: Vec<f32>,
    last_used: u64,
}

/// Lazy mode rasters. Once full, eviction reuses the least recently used
/// allocation. Only amplitudes are cached; gradients exist for the combined
/// field alone.
#[derive(Debug)]
pub struct FieldCache {
    side: usize,
    capacity: usize,
    clock: u64,
    entries: Vec<CacheEntry>,
    rasterizer: Rasterizer,
}

impl FieldCache {
    pub fn for_plate(plate_width: usize, plate_height: usize) -> Self {
        Self::with_capacity(plate_width, plate_height, DEFAULT_CACHE_CAPACITY)
    }

    pub fn with_capacity(plate_width: usize, plate_height: usize, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let side = plate_width.max(plate_height).max(MIN_GRID_SIDE);
        Self {
            side,
            capacity,
            clock: 0,
            entries: Vec::with_capacity(capacity),
            rasterizer: Rasterizer::new(side),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, mode: impl Into<Mode>) -> bool {
        let mode = mode.into();
        self.entries.iter().any(|entry| entry.mode == mode)
    }

    /// Normalized |f| of `mode`, rasterized on first use.
    pub fn amplitudes(&mut self, mode: impl Into<Mode>) -> &[f32] {
        let mode = mode.into();
        self.clock = self.clock.saturating_add(1);
        if let Some(index) = self.entries.iter().position(|entry| entry.mode == mode) {
            self.entries[index].last_used = self.clock;
            return &self.entries[index].amplitudes;
        }
        if self.entries.len() < self.capacity {
            let mut amplitudes = vec![0.0; self.side * self.side];
            self.rasterizer.rasterize(mode, &mut amplitudes);
            self.entries.push(CacheEntry {
                mode,
                amplitudes,
                last_used: self.clock,
            });
            return &self.entries[self.entries.len() - 1].amplitudes;
        }
        let mut oldest = 0;
        for index in 1..self.entries.len() {
            if self.entries[index].last_used < self.entries[oldest].last_used {
                oldest = index;
            }
        }
        let entry = &mut self.entries[oldest];
        entry.mode = mode;
        entry.last_used = self.clock;
        self.rasterizer.rasterize(mode, &mut entry.amplitudes);
        &entry.amplitudes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FieldError {
    #[error("an active field supports at most 8 modes")]
    TooManyModes,
    #[error("mode weights must be finite and nonnegative")]
    InvalidWeight,
    #[error("the active field and mode cache must have the same grid size")]
    GridSizeMismatch,
}

/// An incoherent combination of up to eight modes, rebuilt only on weight changes.
#[derive(Debug)]
pub struct ActiveField {
    grid: FieldGrid,
    modes: [Option<(Mode, f32)>; MAX_ACTIVE_MODES],
    mode_count: usize,
}

impl ActiveField {
    pub fn for_plate(plate_width: usize, plate_height: usize) -> Self {
        Self {
            grid: FieldGrid::for_plate(plate_width, plate_height),
            modes: [None; MAX_ACTIVE_MODES],
            mode_count: 0,
        }
    }

    pub fn grid(&self) -> &FieldGrid {
        &self.grid
    }

    /// Turn the combined figure about the plate centre; this never rebuilds it.
    pub fn set_rotation(&mut self, angle: f32) {
        self.grid.set_rotation(angle);
    }

    /// Return whether the grid changed. No memory is allocated after cache warm-up.
    pub fn set_modes(
        &mut self,
        modes: &[(Mode, f32)],
        cache: &mut FieldCache,
    ) -> Result<bool, FieldError> {
        if modes.len() > MAX_ACTIVE_MODES {
            return Err(FieldError::TooManyModes);
        }
        if modes
            .iter()
            .any(|&(_, weight)| !weight.is_finite() || weight < 0.0)
        {
            return Err(FieldError::InvalidWeight);
        }
        if self.grid.side != cache.side {
            return Err(FieldError::GridSizeMismatch);
        }
        if modes.len() == self.mode_count
            && modes
                .iter()
                .zip(self.modes)
                .all(|(&mode, active)| Some(mode) == active)
        {
            return Ok(false);
        }
        self.modes.fill(None);
        for (destination, &mode) in self.modes.iter_mut().zip(modes) {
            *destination = Some(mode);
        }
        self.mode_count = modes.len();
        self.grid.amplitudes.fill(0.0);
        // Scale first to prevent overflow in w^2 without changing the formula.
        let maximum_weight = modes
            .iter()
            .map(|&(_, weight)| weight)
            .fold(0.0_f32, f32::max);
        let mut total_squared = 0.0;
        if maximum_weight > 0.0 {
            for &(mode, weight) in modes {
                let squared = (weight / maximum_weight).powi(2);
                if squared == 0.0 {
                    continue;
                }
                total_squared += squared;
                for (combined, &amplitude) in
                    self.grid.amplitudes.iter_mut().zip(cache.amplitudes(mode))
                {
                    *combined += squared * amplitude * amplitude;
                }
            }
            for combined in &mut self.grid.amplitudes {
                *combined = (*combined / total_squared).sqrt();
            }
        }
        self.grid.rebuild_gradients();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use crate::config::schema::{Edge, Symmetry};

    use super::*;

    fn mode(n: u8, m: u8) -> Mode {
        Mode::Square(SquareMode::new(n, m, Edge::Free, Symmetry::Anti).unwrap())
    }

    fn drum(n: u8, k: u8) -> Mode {
        Mode::Circle(CircleMode::new(n, k).unwrap())
    }

    #[test]
    fn raster_is_normalized_and_interpolation_matches_grid_points() {
        let grid = FieldGrid::from_mode(mode(2, 5), 100, 120);
        assert_eq!(grid.side(), 256);
        assert_eq!(
            grid.amplitudes().iter().copied().fold(0.0_f32, f32::max),
            1.0
        );
        for row in (0..256).step_by(17) {
            for column in (0..256).step_by(17) {
                let actual = grid.sample(column as f32 / 255.0, row as f32 / 255.0);
                assert!((actual - grid.amplitudes()[row * 256 + column]).abs() < 1e-6);
                assert!((0.0..=1.0).contains(&actual));
            }
        }
        let larger = FieldGrid::from_mode(mode(0, 1), 300, 200);
        assert_eq!(larger.side(), 300);
    }

    #[test]
    fn sampling_is_bilinear_and_clamps_to_the_boundaries() {
        let grid = FieldGrid::from_mode(mode(0, 1), 1, 1);
        let row = 40;
        let column = 120;
        let corners = [
            grid.amplitudes[row * 256 + column],
            grid.amplitudes[row * 256 + column + 1],
            grid.amplitudes[(row + 1) * 256 + column],
            grid.amplitudes[(row + 1) * 256 + column + 1],
        ];
        let center = grid.sample((column as f32 + 0.5) / 255.0, (row as f32 + 0.5) / 255.0);
        assert!((center - corners.iter().sum::<f32>() / 4.0).abs() < 1e-6);
        assert_eq!(grid.sample(-1.0, 2.0), grid.sample(0.0, 1.0));
    }

    #[test]
    fn gradient_matches_the_analytic_direction_away_from_nodes() {
        let grid = FieldGrid::from_mode(mode(0, 1), 160, 160);
        let (_, gx, gy) = grid.sample_with_gradient(0.2, 0.7);
        // Here cos(pi*y) - cos(pi*x) is negative, so |f|'s derivatives reverse.
        let expected_x = -std::f32::consts::PI * (0.2 * std::f32::consts::PI).sin() / 2.0;
        let expected_y = std::f32::consts::PI * (0.7 * std::f32::consts::PI).sin() / 2.0;
        assert!((gx - expected_x).abs() < 1e-3);
        assert!((gy - expected_y).abs() < 1e-3);
    }

    #[test]
    fn trusted_sampling_matches_checked_sampling_at_edges_grid_points_and_interiors() {
        let grid = FieldGrid::from_mode(mode(2, 5), 160, 160);
        for row in (0..256).step_by(17) {
            for column in (0..256).step_by(17) {
                let x = column as f32 / 255.0;
                let y = row as f32 / 255.0;
                assert_eq!(
                    grid.sample_in_plate_with_gradient(x, y),
                    grid.sample_with_gradient(x, y)
                );
            }
        }
        let mut rng = fastrand::Rng::with_seed(42);
        for _ in 0..10_000 {
            let x = rng.f32();
            let y = rng.f32();
            assert_eq!(
                grid.sample_in_plate_with_gradient(x, y),
                grid.sample_with_gradient(x, y)
            );
        }
    }

    #[test]
    fn cache_is_lazy_and_evicts_the_least_recently_used_grid() {
        let mut cache = FieldCache::with_capacity(80, 80, 2);
        assert!(cache.is_empty());
        cache.amplitudes(mode(0, 1));
        cache.amplitudes(mode(0, 2));
        cache.amplitudes(mode(0, 1));
        cache.amplitudes(mode(1, 2));
        assert_eq!(cache.len(), 2);
        assert!(cache.contains(mode(0, 1)));
        assert!(cache.contains(mode(1, 2)));
        assert!(!cache.contains(mode(0, 2)));
    }

    #[test]
    fn active_field_uses_the_incoherent_weight_formula_and_only_rebuilds_on_change() {
        let mut cache = FieldCache::for_plate(160, 160);
        let mut field = ActiveField::for_plate(160, 160);
        let weights = [(mode(0, 1), 3.0), (mode(1, 2), 4.0)];
        assert!(field.set_modes(&weights, &mut cache).unwrap());
        assert!(!field.set_modes(&weights, &mut cache).unwrap());
        for index in [0, 3_456, 30_000, 65_535] {
            let first = cache.amplitudes(weights[0].0)[index];
            let second = cache.amplitudes(weights[1].0)[index];
            let expected = ((9.0 * first * first + 16.0 * second * second) / 25.0).sqrt();
            assert!((field.grid().amplitudes()[index] - expected).abs() < 1e-6);
        }
        assert!(field.set_modes(&[], &mut cache).unwrap());
        assert!(field.grid().amplitudes().iter().all(|&value| value == 0.0));
    }

    #[test]
    fn invalid_active_weights_leave_the_previous_field_intact() {
        let mut cache = FieldCache::for_plate(160, 160);
        let mut field = ActiveField::for_plate(160, 160);
        field.set_modes(&[(mode(0, 1), 1.0)], &mut cache).unwrap();
        let previous = field.grid().sample(0.2, 0.7);
        assert_eq!(
            field.set_modes(&[(mode(0, 1), f32::NAN)], &mut cache),
            Err(FieldError::InvalidWeight)
        );
        assert_eq!(field.grid().sample(0.2, 0.7), previous);
        let mut wrong_size = FieldCache::for_plate(300, 300);
        assert_eq!(
            field.set_modes(&[], &mut wrong_size),
            Err(FieldError::GridSizeMismatch)
        );
        assert_eq!(
            field.set_modes(&[(mode(0, 1), 1.0); 9], &mut cache),
            Err(FieldError::TooManyModes)
        );
    }

    #[test]
    fn drum_raster_is_normalized_and_matches_the_analytic_mode() {
        let grid = FieldGrid::from_mode(drum(2, 2), 160, 160);
        let inside_maximum = (0..256)
            .flat_map(|row| (0..256).map(move |column| (row, column)))
            .filter(|&(row, column)| {
                let (dx, dy) = (column as f32 / 255.0 - 0.5, row as f32 / 255.0 - 0.5);
                (dx * dx + dy * dy).sqrt() <= 0.5
            })
            .map(|(row, column)| grid.amplitudes()[row * 256 + column])
            .fold(0.0_f32, f32::max);
        assert!((inside_maximum - 1.0).abs() < 1e-6);
        assert!(
            grid.amplitudes()
                .iter()
                .all(|value| (0.0..=1.0).contains(value))
        );
        // Compare against the closed form, scaled by its own disc maximum.
        let reference = CircleMode::new(2, 2).unwrap();
        let maximum = disc_maximum(reference);
        for (x, y) in [(0.31, 0.62), (0.7, 0.4), (0.5, 0.2), (0.45, 0.55)] {
            let expected = reference.value(x, y).abs();
            let actual = grid.sample(x, y) * maximum;
            assert!(
                (actual - expected).abs() < 5e-3,
                "({x}, {y}) {actual} vs {expected}"
            );
        }
    }

    fn disc_maximum(mode: CircleMode) -> f32 {
        let mut maximum = 0.0_f32;
        for row in 0..256 {
            for column in 0..256 {
                let (x, y) = (column as f32 / 255.0, row as f32 / 255.0);
                if ((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt() <= 0.5 {
                    maximum = maximum.max(mode.value(x, y).abs());
                }
            }
        }
        maximum
    }

    #[test]
    fn drum_nodal_diameters_and_circles_have_no_amplitude() {
        // J(2,1) has two nodal diameters at 45 and 135 degrees.
        let grid = FieldGrid::from_mode(drum(2, 1), 160, 160);
        for radius in [0.1_f32, 0.2, 0.3, 0.4] {
            let angle = std::f32::consts::FRAC_PI_4;
            let value = grid.sample(0.5 + radius * angle.cos(), 0.5 + radius * angle.sin());
            assert!(value < 0.03, "diameter at radius {radius}: {value}");
        }
        // The nodal circle of J(0,2) lies at j_{0,1} / j_{0,2} of the radius.
        let grid = FieldGrid::from_mode(drum(0, 2), 160, 160);
        let node = 0.5 * 2.404_826 / 5.520_078;
        assert!(grid.sample(0.5 + node, 0.5) < 0.02);
        assert!(grid.sample(0.5 + 0.5 * node, 0.5) > 0.3);
    }

    #[test]
    fn rotation_turns_the_sampled_figure_and_its_gradient() {
        let base = FieldGrid::from_mode(drum(3, 1), 160, 160);
        let mut turned = base.clone();
        let angle = 0.7_f32;
        turned.set_rotation(angle);
        let (cos, sin) = (angle.cos(), angle.sin());
        for (x, y) in [(0.62, 0.55), (0.4, 0.66), (0.55, 0.3)] {
            // Rotating the point by +angle must reproduce the base value there.
            let (dx, dy) = (x - 0.5, y - 0.5);
            let source = (0.5 + cos * dx + sin * dy, 0.5 - sin * dx + cos * dy);
            let (expected, gx, gy) = base.sample_with_gradient(source.0, source.1);
            let (actual, tx, ty) = turned.sample_with_gradient(x, y);
            assert!((actual - expected).abs() < 1e-6);
            assert!((tx - (cos * gx - sin * gy)).abs() < 1e-5);
            assert!((ty - (sin * gx + cos * gy)).abs() < 1e-5);
        }
        turned.set_rotation(f32::NAN);
        assert_eq!(turned.sample(0.62, 0.55), base.sample(0.62, 0.55));
        // A full turn of an axisymmetric mode changes nothing at all.
        let mut axisymmetric = FieldGrid::from_mode(drum(0, 3), 160, 160);
        let before = axisymmetric.sample_with_gradient(0.7, 0.4);
        axisymmetric.set_rotation(1.1);
        let after = axisymmetric.sample_with_gradient(0.7, 0.4);
        assert!((before.0 - after.0).abs() < 2e-3);
    }

    #[test]
    fn active_field_mixes_drum_modes_with_the_same_formula() {
        let mut cache = FieldCache::for_plate(160, 160);
        let mut field = ActiveField::for_plate(160, 160);
        let weights = [(drum(1, 1), 1.0), (drum(0, 2), 1.0)];
        field.set_modes(&weights, &mut cache).unwrap();
        for index in [100, 20_000, 40_000, 65_000] {
            let first = cache.amplitudes(weights[0].0)[index];
            let second = cache.amplitudes(weights[1].0)[index];
            let expected = ((first * first + second * second) / 2.0).sqrt();
            assert!((field.grid().amplitudes()[index] - expected).abs() < 1e-6);
        }
    }
}
