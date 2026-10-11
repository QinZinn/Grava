//! The analytic vibration modes of a circular drum head.
//!
//! `u(r, theta) = J_n(j_{n,k} r) cos(n theta)` on the unit disc. Nodal lines are
//! `n` diameters and `k - 1` circles besides the fixed rim.

use super::{
    bessel::{self, MAX_ORDER, MAX_ZEROS},
    square::ModeError,
};

use std::sync::OnceLock;

/// Modes whose motionless core reaches beyond this fraction of the radius are
/// hollow: see [`CircleMode::dead_radius`].
pub const HOLLOW_RADIUS: f32 = 0.4;
/// Inside the dead core the vibration stays under this share of its maximum.
const DEAD_LEVEL: f32 = 0.05;
/// Radial samples used to measure the dead core.
const DEAD_SAMPLES: usize = 400;

/// A drum mode identified by its angular order `n` and radial index `k`.
/// The zero `j_{n,k}` is looked up on demand, so the type stays `Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CircleMode {
    n: u8,
    k: u8,
}

impl CircleMode {
    /// The lowest drum mode, the axisymmetric fundamental.
    pub const FUNDAMENTAL: Self = Self { n: 0, k: 1 };

    pub fn new(n: u8, k: u8) -> Result<Self, ModeError> {
        if n > MAX_ORDER || k == 0 || k > MAX_ZEROS {
            return Err(ModeError::InvalidDrumMode);
        }
        Ok(Self { n, k })
    }

    pub fn n(self) -> u8 {
        self.n
    }

    pub fn k(self) -> u8 {
        self.k
    }

    /// The scaled argument `j_{n,k}`; also the eigenfrequency ordering key.
    pub fn zero(self) -> f64 {
        // The constructor keeps n and k inside the table.
        bessel::zero(self.n, self.k).unwrap_or(f64::INFINITY)
    }

    /// The radius, as a fraction of the drum's, inside which the mode is almost
    /// motionless: `|J_n(j r)|` stays under 5 % of its maximum. `J_n` is
    /// negligible below its first turning point, so high angular orders have
    /// a large core.
    pub fn dead_radius(self) -> f32 {
        dead_radii()[usize::from(self.n)][usize::from(self.k) - 1]
    }

    /// Whether the motionless core is so large that the mode draws no figure.
    /// Sand scattered into a dead core never moves, so such a mode would show
    /// the initial noise instead of nodal lines ("whispering gallery" modes
    /// such as `J(12, 1)`).
    pub fn is_hollow(self) -> bool {
        self.dead_radius() > HOLLOW_RADIUS
    }

    /// The equivalent square-plate order: nodal lines of the drum are
    /// `pi / j` plate radii apart, as for a square mode of this order.
    pub fn order(self) -> u8 {
        (2.0 * self.zero() / std::f64::consts::PI)
            .ceil()
            .clamp(1.0, 255.0) as u8
    }

    /// The radial factor `J_n(j_{n,k} r)`, also defined beyond the rim.
    pub fn radial(self, r: f32) -> f32 {
        libm::jn(i32::from(self.n), self.zero() * f64::from(r)) as f32
    }

    /// Signed displacement before normalization at plate coordinates in
    /// `0..=1`; the drum is the disc inscribed in the unit square.
    pub fn value(self, x: f32, y: f32) -> f32 {
        let dx = x - 0.5;
        let dy = y - 0.5;
        let r = 2.0 * (dx * dx + dy * dy).sqrt();
        let angular = if self.n == 0 {
            1.0
        } else {
            (f32::from(self.n) * dy.atan2(dx)).cos()
        };
        self.radial(r) * angular
    }
}

/// The dead radius of every mode, measured once on first use.
fn dead_radii() -> &'static [[f32; MAX_ZEROS as usize]; MAX_ORDER as usize + 1] {
    static RADII: OnceLock<[[f32; MAX_ZEROS as usize]; MAX_ORDER as usize + 1]> = OnceLock::new();
    RADII.get_or_init(|| {
        let mut radii = [[0.0; MAX_ZEROS as usize]; MAX_ORDER as usize + 1];
        for (n, row) in radii.iter_mut().enumerate() {
            for (index, radius) in row.iter_mut().enumerate() {
                let mode = CircleMode {
                    n: n as u8,
                    k: index as u8 + 1,
                };
                let samples: Vec<f32> = (0..=DEAD_SAMPLES)
                    .map(|step| mode.radial(step as f32 / DEAD_SAMPLES as f32).abs())
                    .collect();
                let maximum = samples.iter().copied().fold(0.0_f32, f32::max);
                *radius = samples
                    .iter()
                    .position(|&value| value >= DEAD_LEVEL * maximum)
                    .map_or(1.0, |step| step as f32 / DEAD_SAMPLES as f32);
            }
        }
        radii
    })
}

impl std::fmt::Display for CircleMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "J({},{})", self.n, self.k)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn rim_is_a_node_and_angular_order_counts_diameters() {
        let mode = CircleMode::new(3, 2).unwrap();
        for step in 0..32 {
            let angle = step as f32 * 0.37;
            let (x, y) = (0.5 + 0.5 * angle.cos(), 0.5 + 0.5 * angle.sin());
            assert!(mode.value(x, y).abs() < 1e-4, "rim at {angle}");
        }
        // cos(3 theta) changes sign across a nodal diameter at theta = pi/6.
        let on_axis = mode.value(0.5 + 0.2, 0.5);
        let past_node = {
            let angle = std::f32::consts::FRAC_PI_6 + 0.1;
            mode.value(0.5 + 0.2 * angle.cos(), 0.5 + 0.2 * angle.sin())
        };
        assert!(on_axis * past_node < 0.0);
    }

    #[test]
    fn radial_index_counts_nodal_circles() {
        let mode = CircleMode::new(0, 3).unwrap();
        let sign_changes = (1..1000)
            .map(|index| mode.radial(index as f32 / 1000.0))
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|pair| pair[0] * pair[1] < 0.0)
            .count();
        // Two interior nodal circles; the third zero is the rim itself.
        assert_eq!(sign_changes, 2);
    }

    #[test]
    fn order_tracks_nodal_line_spacing() {
        assert_eq!(CircleMode::FUNDAMENTAL.order(), 2);
        assert!(CircleMode::new(0, 8).unwrap().order() > CircleMode::new(0, 2).unwrap().order());
        assert!(CircleMode::new(16, 8).unwrap().order() > 16);
    }

    #[test]
    fn whispering_gallery_modes_are_hollow_and_classic_ones_are_not() {
        let hollow = |n, k| CircleMode::new(n, k).unwrap().is_hollow();
        for (n, k) in [
            (0, 1),
            (1, 1),
            (2, 1),
            (3, 1),
            (4, 1),
            (5, 1),
            (6, 1),
            (0, 8),
            (4, 3),
            (8, 2),
        ] {
            assert!(!hollow(n, k), "J({n},{k})");
        }
        for (n, k) in [(10, 1), (12, 1), (16, 1), (14, 2), (16, 2)] {
            assert!(hollow(n, k), "J({n},{k})");
        }
        // Axisymmetric modes have no core at all; the measured core of J(12,1)
        // really is quiet, and quiet only up to its edge.
        assert_eq!(CircleMode::new(0, 3).unwrap().dead_radius(), 0.0);
        let mode = CircleMode::new(12, 1).unwrap();
        let radius = mode.dead_radius();
        assert!((0.45..0.55).contains(&radius), "{radius}");
        let outside = (0..=100)
            .map(|i| mode.radial(i as f32 / 100.0).abs())
            .fold(0.0, f32::max);
        assert!(mode.radial(radius * 0.9).abs() < 0.05 * outside);
        assert!(mode.radial(radius + 0.02).abs() >= 0.04 * outside);
    }

    #[test]
    fn invalid_orders_are_rejected() {
        for (n, k) in [(17, 1), (0, 0), (0, 9)] {
            assert_eq!(CircleMode::new(n, k), Err(ModeError::InvalidDrumMode));
        }
        assert_eq!(CircleMode::new(2, 5).unwrap().to_string(), "J(2,5)");
    }
}
