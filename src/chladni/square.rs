//! The analytic vibration modes of a square plate.

use crate::config::schema::{Edge, Symmetry};

/// A nondegenerate square mode. Mixed symmetry is resolved by the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SquareMode {
    n: u8,
    m: u8,
    edge: Edge,
    symmetry: Symmetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ModeError {
    #[error("square mode orders must be at most 16")]
    OrderTooLarge,
    #[error("fixed-edge square modes require both orders to be at least 1")]
    FixedZeroOrder,
    #[error("anti-symmetric square modes require different orders")]
    IdenticalAntiOrders,
    #[error("mixed symmetry must be resolved to anti or sym by the mode catalog")]
    MixedSymmetry,
    #[error("drum modes require n in 0..16 and k in 1..8")]
    InvalidDrumMode,
}

impl SquareMode {
    pub fn new(n: u8, m: u8, edge: Edge, symmetry: Symmetry) -> Result<Self, ModeError> {
        if n > 16 || m > 16 {
            return Err(ModeError::OrderTooLarge);
        }
        if edge == Edge::Fixed && (n == 0 || m == 0) {
            return Err(ModeError::FixedZeroOrder);
        }
        if symmetry == Symmetry::Mixed {
            return Err(ModeError::MixedSymmetry);
        }
        if symmetry == Symmetry::Anti && n == m {
            return Err(ModeError::IdenticalAntiOrders);
        }
        Ok(Self {
            n,
            m,
            edge,
            symmetry,
        })
    }

    pub fn n(self) -> u8 {
        self.n
    }

    pub fn m(self) -> u8 {
        self.m
    }

    pub fn edge(self) -> Edge {
        self.edge
    }

    pub fn symmetry(self) -> Symmetry {
        self.symmetry
    }

    pub fn order(self) -> u8 {
        self.n.max(self.m)
    }

    pub fn eigenvalue(self) -> u16 {
        u16::from(self.n).pow(2) + u16::from(self.m).pow(2)
    }

    /// Signed displacement before normalization. Coordinates are plate units.
    pub fn value(self, x: f32, y: f32) -> f32 {
        self.combine(
            self.axis(self.n, x),
            self.axis(self.m, x),
            self.axis(self.n, y),
            self.axis(self.m, y),
        )
    }

    pub(super) fn axis(self, order: u8, coordinate: f32) -> f32 {
        let phase = f32::from(order) * std::f32::consts::PI * coordinate;
        match self.edge {
            Edge::Free => phase.cos(),
            // Enforce the exact boundary condition despite finite-precision sin(k*pi).
            Edge::Fixed if coordinate == 0.0 || coordinate == 1.0 => 0.0,
            Edge::Fixed => phase.sin(),
        }
    }

    pub(super) fn combine(self, nx: f32, mx: f32, ny: f32, my: f32) -> f32 {
        let first = nx * my;
        let second = mx * ny;
        match self.symmetry {
            Symmetry::Anti => first - second,
            Symmetry::Sym => first + second,
            // Constructor validation ensures that a mode has resolved symmetry.
            Symmetry::Mixed => unreachable!("a square mode has resolved symmetry"),
        }
    }
}

impl std::fmt::Display for SquareMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "({},{})", self.n, self.m)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn anti_mode_is_exactly_zero_on_the_diagonal() {
        for edge in [Edge::Free, Edge::Fixed] {
            let mode = SquareMode::new(2, 5, edge, Symmetry::Anti).unwrap();
            for index in 0..=1_000 {
                let coordinate = index as f32 / 1_000.0;
                assert_eq!(mode.value(coordinate, coordinate), 0.0);
            }
        }
    }

    #[test]
    fn fixed_modes_obey_all_four_boundaries() {
        let mode = SquareMode::new(2, 5, Edge::Fixed, Symmetry::Sym).unwrap();
        for index in 0..=100 {
            let coordinate = index as f32 / 100.0;
            assert_eq!(mode.value(0.0, coordinate), 0.0);
            assert_eq!(mode.value(1.0, coordinate), 0.0);
            assert_eq!(mode.value(coordinate, 0.0), 0.0);
            assert_eq!(mode.value(coordinate, 1.0), 0.0);
        }
    }

    #[test]
    fn invalid_or_degenerate_modes_are_rejected() {
        assert_eq!(
            SquareMode::new(1, 1, Edge::Free, Symmetry::Anti),
            Err(ModeError::IdenticalAntiOrders)
        );
        assert_eq!(
            SquareMode::new(0, 1, Edge::Fixed, Symmetry::Sym),
            Err(ModeError::FixedZeroOrder)
        );
        assert_eq!(
            SquareMode::new(1, 17, Edge::Free, Symmetry::Sym),
            Err(ModeError::OrderTooLarge)
        );
        assert_eq!(
            SquareMode::new(1, 2, Edge::Free, Symmetry::Mixed),
            Err(ModeError::MixedSymmetry)
        );
    }
}
