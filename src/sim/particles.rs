//! Seeded sand motion on a normalized square plate or circular drum head.

use crate::chladni::field::FieldGrid;
use crate::config::schema::{ParticlesConfig, Shape};

/// Struct-of-arrays particle storage in plate coordinates, each in `0..=1`.
/// On a drum head the sand stays inside the disc of radius 0.5 about the
/// plate centre `(0.5, 0.5)`.
///
/// Configuration and resizing may allocate. [`Self::step`] and [`Self::scatter`]
/// reuse their existing storage. Velocities are allocated only when inertia is
/// enabled through [`Self::configure`].
pub struct Particles {
    x: Vec<f32>,
    y: Vec<f32>,
    velocities: Option<Velocities>,
    rng: fastrand::Rng,
    shape: Shape,
}

struct Velocities {
    x: Vec<f32>,
    y: Vec<f32>,
}

impl Particles {
    /// Scatter `count` grains uniformly over a square plate, using a private
    /// deterministic RNG.
    #[must_use]
    pub fn new(count: usize, seed: u64) -> Self {
        Self::with_shape(count, seed, Shape::Square)
    }

    /// Scatter `count` grains uniformly over a plate of the given shape.
    #[must_use]
    pub fn with_shape(count: usize, seed: u64, shape: Shape) -> Self {
        let mut particles = Self {
            x: vec![0.0; count],
            y: vec![0.0; count],
            velocities: None,
            rng: fastrand::Rng::with_seed(seed),
            shape,
        };
        particles.scatter();
        particles
    }

    /// Construct and prepare optional velocity storage for the configuration.
    #[must_use]
    pub fn new_with_config(count: usize, seed: u64, config: &ParticlesConfig) -> Self {
        Self::new_with_shape(count, seed, config, Shape::Square)
    }

    /// Like [`Self::new_with_config`], for a plate of the given shape.
    #[must_use]
    pub fn new_with_shape(count: usize, seed: u64, config: &ParticlesConfig, shape: Shape) -> Self {
        let mut particles = Self::with_shape(count, seed, shape);
        particles.configure(config);
        particles
    }

    #[must_use]
    pub fn shape(&self) -> Shape {
        self.shape
    }

    /// Change the plate shape and scatter the sand over the new plate.
    pub fn set_shape(&mut self, shape: Shape) {
        if shape != self.shape {
            self.shape = shape;
            self.scatter();
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.x.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.x.is_empty()
    }

    #[must_use]
    pub fn x(&self) -> &[f32] {
        &self.x
    }

    #[must_use]
    pub fn y(&self) -> &[f32] {
        &self.y
    }

    /// Enable or release optional velocity arrays outside the simulation loop.
    ///
    /// Call this after changing `particles.inertia`. Passing a configuration to
    /// [`Self::step`] never allocates missing storage itself.
    pub fn configure(&mut self, config: &ParticlesConfig) {
        if config.inertia > 0.0 && self.velocities.is_none() {
            self.velocities = Some(Velocities {
                x: vec![0.0; self.len()],
                y: vec![0.0; self.len()],
            });
        } else if config.inertia <= 0.0 {
            self.velocities = None;
        }
    }

    /// Change the count, preserving existing positions and scattering new grains.
    pub fn resize_count(&mut self, count: usize) {
        let previous_count = self.len();
        self.x.resize(count, 0.0);
        self.y.resize(count, 0.0);
        for index in previous_count..count {
            let (x, y) = random_position(&mut self.rng, self.shape);
            self.x[index] = x;
            self.y[index] = y;
        }
        if let Some(velocities) = &mut self.velocities {
            velocities.x.resize(count, 0.0);
            velocities.y.resize(count, 0.0);
        }
    }

    /// Re-scatter all grains uniformly and clear their momentum.
    pub fn scatter(&mut self) {
        for (x, y) in self.x.iter_mut().zip(&mut self.y) {
            (*x, *y) = random_position(&mut self.rng, self.shape);
        }
        if let Some(velocities) = &mut self.velocities {
            velocities.x.fill(0.0);
            velocities.y.fill(0.0);
        }
    }

    /// Advance the sand without allocating or reading a clock.
    ///
    /// `step` in the configuration is measured in sub-pixels. Width and height
    /// convert it to normalized plate coordinates. Brownian motion scales with
    /// the square root of elapsed time; settling and momentum use elapsed time.
    /// A silent frame freezes positions and the RNG exactly, including inertia.
    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        field: &FieldGrid,
        energy: f32,
        onset: f32,
        dt: f32,
        config: &ParticlesConfig,
        subpixel_width: usize,
        subpixel_height: usize,
    ) {
        let energy = nonnegative(energy);
        let onset = nonnegative(onset).min(1.0);
        if (energy == 0.0 && onset == 0.0)
            || !dt.is_finite()
            || dt <= 0.0
            || subpixel_width == 0
            || subpixel_height == 0
        {
            return;
        }
        let frame_time = dt * 60.0;
        if !frame_time.is_finite() {
            return;
        }
        let shaking = energy * nonnegative(config.jitter);
        let motion = Motion {
            field,
            step_x: nonnegative(config.step) / subpixel_width as f32,
            step_y: nonnegative(config.step) / subpixel_height as f32,
            shake: shaking * frame_time.sqrt(),
            settle: nonnegative(config.settle) * shaking * frame_time,
            kick: nonnegative(config.kick) * onset,
        };
        // Validate the derived scales once rather than checking two resulting
        // displacements for every grain in the pure random-walk hot path.
        if !motion.has_finite_displacements() {
            return;
        }

        // Exponential damping gives the same momentum retention per second at
        // other frame rates as at the configured 60 Hz reference.
        let inertia = nonnegative(config.inertia).min(1.0).powf(frame_time);
        match self.shape {
            Shape::Square => self.advance(&motion, inertia, |x, y, dx, dy| {
                let (next_x, direction_x) = reflect(x + dx);
                let (next_y, direction_y) = reflect(y + dy);
                (next_x, next_y, dx * direction_x, dy * direction_y)
            }),
            Shape::Circle => self.advance(&motion, inertia, |x, y, dx, dy| {
                let (next_x, next_y, bounced) = reflect_disc(x + dx, y + dy);
                if bounced {
                    // Momentum continues along the displacement actually made.
                    (next_x, next_y, next_x - x, next_y - y)
                } else {
                    (next_x, next_y, dx, dy)
                }
            }),
        }
    }

    /// Move every grain by its displacement (plus carried momentum) and let
    /// `bounce` fold the result back into the plate. `bounce` maps
    /// `(x, y, dx, dy)` to the new position and the momentum to carry.
    #[inline(always)]
    fn advance(
        &mut self,
        motion: &Motion<'_>,
        inertia: f32,
        bounce: impl Fn(f32, f32, f32, f32) -> (f32, f32, f32, f32),
    ) {
        let Self {
            x,
            y,
            velocities,
            rng,
            ..
        } = self;
        if let Some(velocities) = velocities {
            for (((x, y), vx), vy) in x
                .iter_mut()
                .zip(y.iter_mut())
                .zip(&mut velocities.x)
                .zip(&mut velocities.y)
            {
                let (dx, dy) = motion.displacement(*x, *y, rng);
                let dx = finite_or_zero(dx + *vx * inertia);
                let dy = finite_or_zero(dy + *vy * inertia);
                (*x, *y, *vx, *vy) = bounce(*x, *y, dx, dy);
            }
        } else {
            for (x, y) in x.iter_mut().zip(y.iter_mut()) {
                let (dx, dy) = motion.displacement(*x, *y, rng);
                (*x, *y, _, _) = bounce(*x, *y, dx, dy);
            }
        }
    }
}

/// A uniform random point of the plate; the drum uses rejection sampling.
fn random_position(rng: &mut fastrand::Rng, shape: Shape) -> (f32, f32) {
    loop {
        let (x, y) = (rng.f32(), rng.f32());
        if shape == Shape::Square || (x - 0.5) * (x - 0.5) + (y - 0.5) * (y - 0.5) <= 0.25 {
            return (x, y);
        }
    }
}

struct Motion<'a> {
    field: &'a FieldGrid,
    step_x: f32,
    step_y: f32,
    shake: f32,
    settle: f32,
    kick: f32,
}

impl Motion<'_> {
    fn has_finite_displacements(&self) -> bool {
        // Our uniform mantissas produce multiples of 2^-22. Marsaglia's squared
        // radius is therefore at least 2^-44, bounding each Gaussian sample
        // below 8; use 16 for additional floating-point rounding headroom.
        let unscaled_bound =
            16.0 * f64::from(self.shake) + f64::from(self.settle) + 32.0 * f64::from(self.kick);
        let limit = f64::from(f32::MAX) * 0.25;
        unscaled_bound <= limit
            && unscaled_bound * f64::from(self.step_x.max(self.step_y)) <= limit
            // Drift divides by a gradient length greater than EPSILON before
            // multiplying by its components; guard that intermediate as well.
            && f64::from(self.settle) <= limit * f64::from(f32::EPSILON)
    }

    #[inline]
    fn displacement(&self, x: f32, y: f32, rng: &mut fastrand::Rng) -> (f32, f32) {
        let (amplitude, gradient_x, gradient_y) = self.field.sample_in_plate_with_gradient(x, y);
        // FieldGrid construction and bilinear interpolation already guarantee
        // finite amplitude in [0, 1]; all positions were scattered or reflected.
        let (random_x, random_y) = gaussian_pair(rng);
        let mut dx = random_x * self.shake * amplitude;
        let mut dy = random_y * self.shake * amplitude;
        let gradient_length = (gradient_x * gradient_x + gradient_y * gradient_y).sqrt();
        if gradient_length > f32::EPSILON && gradient_length.is_finite() {
            let drift = self.settle * amplitude / gradient_length;
            dx -= gradient_x * drift;
            dy -= gradient_y * drift;
        }
        if self.kick > 0.0 {
            let (kick_x, kick_y) = gaussian_pair(rng);
            let strength = self.kick * (0.3 + amplitude);
            dx += kick_x * strength;
            dy += kick_y * strength;
        }
        (dx * self.step_x, dy * self.step_y)
    }
}

/// Marsaglia's polar transform, producing independent standard normal samples.
#[inline]
fn gaussian_pair(rng: &mut fastrand::Rng) -> (f32, f32) {
    loop {
        // Each half supplies an independent 23-bit uniform mantissa. Building
        // floats in [2, 4) and subtracting 3 avoids integer-to-float conversions
        // and consumes one random word for both polar coordinates.
        let random = rng.u64(..);
        let x = f32::from_bits(((random as u32) >> 9) | 0x4000_0000) - 3.0;
        let y = f32::from_bits(((random >> 32) as u32 >> 9) | 0x4000_0000) - 3.0;
        let radius_squared = x * x + y * y;
        if radius_squared > 0.0 && radius_squared < 1.0 {
            let factor = (-2.0 * radius_squared.ln() / radius_squared).sqrt();
            return (x * factor, y * factor);
        }
    }
}

/// Fold any finite displacement into the plate, including multiple crossings.
/// The second value reverses momentum on an odd number of reflections.
#[inline]
fn reflect(position: f32) -> (f32, f32) {
    if (0.0..=1.0).contains(&position) {
        return (position, 1.0);
    }
    if !position.is_finite() {
        return (0.5, 0.0);
    }
    let wrapped = position.rem_euclid(2.0);
    if wrapped <= 1.0 {
        (wrapped, 1.0)
    } else {
        (2.0 - wrapped, -1.0)
    }
}

/// Fold a point back inside the drum of radius 0.5 about the plate centre by
/// reflecting the radius at the rim. The flag reports whether it moved.
#[inline]
fn reflect_disc(x: f32, y: f32) -> (f32, f32, bool) {
    let dx = x - 0.5;
    let dy = y - 0.5;
    let radius_squared = dx * dx + dy * dy;
    if radius_squared <= 0.25 {
        return (x, y, false);
    }
    if !radius_squared.is_finite() {
        return (0.5, 0.5, true);
    }
    let radius = radius_squared.sqrt();
    // The rim reflects the radius; very long steps wrap with period 1.
    let wrapped = radius.rem_euclid(1.0);
    let folded = if wrapped <= 0.5 {
        wrapped
    } else {
        1.0 - wrapped
    };
    let scale = folded / radius;
    // Rounding can leave a rim point a hair outside the unit square.
    (
        (0.5 + dx * scale).clamp(0.0, 1.0),
        (0.5 + dy * scale).clamp(0.0, 1.0),
        true,
    )
}

#[inline]
fn nonnegative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[inline]
fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::chladni::SquareMode;
    use crate::config::schema::{Edge, Symmetry};

    fn field() -> FieldGrid {
        let mode = SquareMode::new(2, 5, Edge::Free, Symmetry::Anti).unwrap();
        FieldGrid::from_mode(mode, 160, 160)
    }

    fn mean_amplitude(particles: &Particles, field: &FieldGrid) -> f32 {
        particles
            .x()
            .iter()
            .zip(particles.y())
            .map(|(&x, &y)| field.sample(x, y))
            .sum::<f32>()
            / particles.len() as f32
    }

    #[test]
    fn same_seed_produces_the_same_positions_and_motion() {
        let field = field();
        let config = ParticlesConfig::default();
        let mut first = Particles::new(2_000, 42);
        let mut second = Particles::new(2_000, 42);
        assert_eq!(first.x(), second.x());
        assert_eq!(first.y(), second.y());
        for _ in 0..30 {
            first.step(&field, 0.8, 0.5, 1.0 / 60.0, &config, 160, 160);
            second.step(&field, 0.8, 0.5, 1.0 / 60.0, &config, 160, 160);
        }
        assert_eq!(first.x(), second.x());
        assert_eq!(first.y(), second.y());
        assert_ne!(first.x(), Particles::new(2_000, 43).x());
    }

    #[test]
    fn scatter_is_uniform_and_stays_inside_the_plate() {
        let mut particles = Particles::new(20_000, 17);
        for axis in [particles.x(), particles.y()] {
            assert!(axis.iter().all(|p| (0.0..=1.0).contains(p)));
            let mean = axis.iter().sum::<f32>() / axis.len() as f32;
            assert!((mean - 0.5).abs() < 0.01);
            let mut bins = [0; 10];
            for &position in axis {
                bins[(position * 10.0) as usize] += 1;
            }
            assert!(bins.iter().all(|&count| (1_800..2_200).contains(&count)));
        }
        let previous = particles.x().to_vec();
        particles.scatter();
        assert_ne!(previous, particles.x());
    }

    #[test]
    fn silence_freezes_positions_momentum_and_rng() {
        let field = field();
        let config = ParticlesConfig {
            inertia: 0.75,
            ..ParticlesConfig::default()
        };
        let mut first = Particles::new_with_config(2_000, 4, &config);
        let mut second = Particles::new_with_config(2_000, 4, &config);
        first.step(&field, 0.8, 0.0, 1.0 / 60.0, &config, 160, 160);
        second.step(&field, 0.8, 0.0, 1.0 / 60.0, &config, 160, 160);
        let previous_x = first.x().to_vec();
        let previous_y = first.y().to_vec();
        for _ in 0..20 {
            first.step(&field, 0.0, 0.0, 1.0 / 60.0, &config, 160, 160);
        }
        assert_eq!(first.x(), previous_x);
        assert_eq!(first.y(), previous_y);
        first.step(&field, 0.8, 0.0, 1.0 / 60.0, &config, 160, 160);
        second.step(&field, 0.8, 0.0, 1.0 / 60.0, &config, 160, 160);
        assert_eq!(first.x(), second.x());
        assert_eq!(first.y(), second.y());
    }

    #[test]
    fn default_sand_converges_to_the_nodal_lines() {
        let field = field();
        let config = ParticlesConfig::default();
        let mut particles = Particles::new(20_000, 123);
        let initial = mean_amplitude(&particles, &field);
        for _ in 0..300 {
            particles.step(&field, 0.8, 0.0, 1.0 / 60.0, &config, 160, 160);
        }
        let final_mean = mean_amplitude(&particles, &field);
        assert!(
            final_mean < initial * 0.35,
            "mean amplitude {initial:.5} -> {final_mean:.5}"
        );
    }

    #[test]
    fn reflection_handles_arbitrary_crossings() {
        for (position, expected, direction) in [
            (-5.25, 0.75, 1.0),
            (-2.25, 0.25, -1.0),
            (-0.25, 0.25, -1.0),
            (0.25, 0.25, 1.0),
            (1.25, 0.75, -1.0),
            (2.25, 0.25, 1.0),
            (7.25, 0.75, -1.0),
        ] {
            assert_eq!(reflect(position), (expected, direction));
        }
        assert_eq!(reflect(f32::NAN), (0.5, 0.0));
    }

    #[test]
    fn large_motion_and_inertia_never_escape_or_become_nonfinite() {
        let field = field();
        let config = ParticlesConfig {
            step: 50.0,
            inertia: 0.9,
            ..ParticlesConfig::default()
        };
        let mut particles = Particles::new_with_config(1_000, 44, &config);
        for _ in 0..300 {
            particles.step(&field, 3.0, 1.0, 0.5, &config, 2, 2);
        }
        assert!(particles.x().iter().all(|p| (0.0..=1.0).contains(p)));
        assert!(particles.y().iter().all(|p| (0.0..=1.0).contains(p)));
    }

    #[test]
    fn extreme_finite_configuration_keeps_positions_finite() {
        let field = field();
        let configurations = [
            ParticlesConfig {
                step: f32::MAX,
                ..ParticlesConfig::default()
            },
            ParticlesConfig {
                jitter: f32::MAX,
                settle: f32::MAX,
                ..ParticlesConfig::default()
            },
            ParticlesConfig {
                kick: f32::MAX,
                inertia: 1.0,
                ..ParticlesConfig::default()
            },
        ];
        for config in configurations {
            let mut particles = Particles::new_with_config(256, 14, &config);
            for _ in 0..10 {
                particles.step(&field, 1.5, 1.0, 1.0 / 60.0, &config, 160, 160);
            }
            assert!(particles.x().iter().all(|p| (0.0..=1.0).contains(p)));
            assert!(particles.y().iter().all(|p| (0.0..=1.0).contains(p)));
        }
    }

    #[test]
    fn resizing_preserves_grains_and_prepares_optional_momentum() {
        let config = ParticlesConfig {
            inertia: 0.5,
            ..ParticlesConfig::default()
        };
        let mut particles = Particles::new(10, 6);
        assert!(particles.velocities.is_none());
        particles.configure(&config);
        let previous_x = particles.x().to_vec();
        particles.resize_count(20);
        assert_eq!(&particles.x()[..10], previous_x);
        assert_eq!(particles.velocities.as_ref().unwrap().x.len(), 20);
        particles.resize_count(0);
        assert!(particles.is_empty());
        particles.configure(&ParticlesConfig::default());
        assert!(particles.velocities.is_none());
    }

    #[test]
    fn equal_elapsed_time_has_equal_brownian_variance() {
        let constant_mode = SquareMode::new(0, 0, Edge::Free, Symmetry::Sym).unwrap();
        let field = FieldGrid::from_mode(constant_mode, 160, 160);
        let config = ParticlesConfig {
            settle: 0.0,
            ..ParticlesConfig::default()
        };
        let mut full_frame = Particles::new(40_000, 123);
        let mut half_frames = Particles::new(40_000, 123);
        for particles in [&mut full_frame, &mut half_frames] {
            particles.x.fill(0.5);
            particles.y.fill(0.5);
        }
        full_frame.step(&field, 0.8, 0.0, 1.0 / 60.0, &config, 160, 160);
        for _ in 0..2 {
            half_frames.step(&field, 0.8, 0.0, 1.0 / 120.0, &config, 160, 160);
        }
        let variance = |particles: &Particles| {
            particles
                .x()
                .iter()
                .chain(particles.y())
                .map(|position| (position - 0.5).powi(2))
                .sum::<f32>()
                / (2 * particles.len()) as f32
        };
        let ratio = variance(&half_frames) / variance(&full_frame);
        assert!((ratio - 1.0).abs() < 0.03, "variance ratio: {ratio}");
    }

    #[test]
    fn gaussian_samples_have_unit_variance_and_independent_axes() {
        let mut rng = fastrand::Rng::with_seed(581);
        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        let mut squared_x = 0.0;
        let mut squared_y = 0.0;
        let mut product = 0.0;
        let mut tails = 0;
        for _ in 0..100_000 {
            let (x, y) = gaussian_pair(&mut rng);
            let (x, y) = (f64::from(x), f64::from(y));
            sum_x += x;
            sum_y += y;
            squared_x += x * x;
            squared_y += y * y;
            product += x * y;
            tails += usize::from(x.abs() > 2.0) + usize::from(y.abs() > 2.0);
        }
        assert!((sum_x / 100_000.0).abs() < 0.01);
        assert!((sum_y / 100_000.0).abs() < 0.01);
        assert!((squared_x / 100_000.0 - 1.0).abs() < 0.02);
        assert!((squared_y / 100_000.0 - 1.0).abs() < 0.02);
        assert!((product / 100_000.0).abs() < 0.01);
        // A standard Gaussian has about 4.55% of its mass beyond +/-2 sigma.
        assert!((tails as f64 / 200_000.0 - 0.0455).abs() < 0.003);
    }

    fn drum_field() -> FieldGrid {
        FieldGrid::from_mode(crate::chladni::CircleMode::new(3, 2).unwrap(), 160, 160)
    }

    fn inside_disc(particles: &Particles) -> bool {
        particles
            .x()
            .iter()
            .zip(particles.y())
            .all(|(&x, &y)| (x - 0.5).powi(2) + (y - 0.5).powi(2) <= 0.25 + 1e-6)
    }

    #[test]
    fn drum_scatter_is_uniform_over_the_disc() {
        let particles = Particles::with_shape(40_000, 9, Shape::Circle);
        assert!(inside_disc(&particles));
        // Uniform over a disc: a quarter of the grains lie within half the radius.
        let inner = particles
            .x()
            .iter()
            .zip(particles.y())
            .filter(|&(&x, &y)| (x - 0.5).powi(2) + (y - 0.5).powi(2) <= 0.0625)
            .count();
        assert!((inner as f32 / 40_000.0 - 0.25).abs() < 0.01, "{inner}");
        let mean_x = particles.x().iter().sum::<f32>() / 40_000.0;
        assert!((mean_x - 0.5).abs() < 0.01);
        let again = Particles::with_shape(40_000, 9, Shape::Circle);
        assert_eq!(particles.x(), again.x());
    }

    #[test]
    fn drum_sand_never_leaves_the_rim_even_with_momentum_and_huge_steps() {
        let field = drum_field();
        for config in [
            ParticlesConfig::default(),
            ParticlesConfig {
                inertia: 0.9,
                step: 20.0,
                ..ParticlesConfig::default()
            },
            ParticlesConfig {
                step: 400.0,
                kick: 5.0,
                ..ParticlesConfig::default()
            },
        ] {
            let mut particles = Particles::new_with_shape(2_000, 3, &config, Shape::Circle);
            for _ in 0..120 {
                particles.step(&field, 1.5, 1.0, 1.0 / 60.0, &config, 160, 160);
                assert!(inside_disc(&particles));
            }
            assert!(
                particles
                    .x()
                    .iter()
                    .all(|p| p.is_finite() && (0.0..=1.0).contains(p))
            );
        }
    }

    #[test]
    fn drum_sand_converges_to_the_nodal_lines() {
        let field = drum_field();
        let config = ParticlesConfig::default();
        let mut particles = Particles::with_shape(20_000, 123, Shape::Circle);
        let initial = mean_amplitude(&particles, &field);
        for _ in 0..300 {
            particles.step(&field, 0.8, 0.0, 1.0 / 60.0, &config, 160, 160);
        }
        let final_mean = mean_amplitude(&particles, &field);
        assert!(
            final_mean < initial * 0.35,
            "mean amplitude {initial:.5} -> {final_mean:.5}"
        );
        assert!(inside_disc(&particles));
    }

    #[test]
    fn rotated_drum_sand_follows_the_turned_figure() {
        let mut field = drum_field();
        field.set_rotation(0.9);
        let config = ParticlesConfig::default();
        let mut particles = Particles::with_shape(20_000, 77, Shape::Circle);
        let initial = mean_amplitude(&particles, &field);
        for _ in 0..300 {
            particles.step(&field, 0.8, 0.0, 1.0 / 60.0, &config, 160, 160);
        }
        assert!(mean_amplitude(&particles, &field) < initial * 0.35);
    }

    #[test]
    fn disc_reflection_folds_the_radius_at_the_rim() {
        assert_eq!(reflect_disc(0.5, 0.5), (0.5, 0.5, false));
        assert_eq!(reflect_disc(0.9, 0.5), (0.9, 0.5, false));
        // 0.6 from the centre reflects to 0.4 on the same ray.
        let (x, y, bounced) = reflect_disc(1.1, 0.5);
        assert!(bounced && (x - 0.9).abs() < 1e-6 && (y - 0.5).abs() < 1e-6);
        let (x, y, _) = reflect_disc(0.5, -0.1);
        assert!((y - 0.1).abs() < 1e-6 && (x - 0.5).abs() < 1e-6);
        // Corners and very long steps land inside as well.
        for (px, py) in [(1.0, 1.0), (-7.3, 4.1), (1e6, -1e6), (0.5, 3.0)] {
            let (x, y, bounced) = reflect_disc(px, py);
            assert!(bounced && (x - 0.5).powi(2) + (y - 0.5).powi(2) <= 0.25 + 1e-6);
        }
        // Non-finite positions return to the centre rather than propagating.
        assert_eq!(reflect_disc(f32::NAN, 0.5), (0.5, 0.5, true));
        assert_eq!(reflect_disc(f32::INFINITY, 0.5), (0.5, 0.5, true));
    }

    #[test]
    fn changing_shape_rescatters_inside_the_new_plate() {
        let mut particles = Particles::new(5_000, 1);
        assert!(!inside_disc(&particles));
        particles.set_shape(Shape::Circle);
        assert_eq!(particles.shape(), Shape::Circle);
        assert!(inside_disc(&particles));
        particles.resize_count(8_000);
        assert!(inside_disc(&particles));
        let before = particles.x().to_vec();
        particles.set_shape(Shape::Circle);
        assert_eq!(particles.x(), before);
    }
}
