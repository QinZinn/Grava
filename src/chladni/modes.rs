//! Stable mode ordering and the size-dependent limit on reachable modes.

use crate::config::schema::{Edge, MaxOrder, PlateConfig, Shape, Symmetry};

use super::{
    bessel::{MAX_ORDER, MAX_ZEROS},
    circle::CircleMode,
    square::SquareMode,
};

/// One vibration mode of either plate shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Square(SquareMode),
    Circle(CircleMode),
}

impl Mode {
    pub fn shape(self) -> Shape {
        match self {
            Self::Square(_) => Shape::Square,
            Self::Circle(_) => Shape::Circle,
        }
    }

    /// `n` of a square mode `(n, m)` or of a drum mode `J(n, k)`.
    pub fn first(self) -> u8 {
        match self {
            Self::Square(mode) => mode.n(),
            Self::Circle(mode) => mode.n(),
        }
    }

    /// `m` of a square mode `(n, m)` or `k` of a drum mode `J(n, k)`.
    pub fn second(self) -> u8 {
        match self {
            Self::Square(mode) => mode.m(),
            Self::Circle(mode) => mode.k(),
        }
    }

    /// Equivalent square order, used to decide whether a size can show the mode.
    pub fn order(self) -> u8 {
        match self {
            Self::Square(mode) => mode.order(),
            Self::Circle(mode) => mode.order(),
        }
    }
}

impl From<SquareMode> for Mode {
    fn from(mode: SquareMode) -> Self {
        Self::Square(mode)
    }
}

impl From<CircleMode> for Mode {
    fn from(mode: CircleMode) -> Self {
        Self::Circle(mode)
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Square(mode) => mode.fmt(formatter),
            Self::Circle(mode) => mode.fmt(formatter),
        }
    }
}

/// The full catalog is independent of the terminal and plate dimensions.
#[derive(Debug, Clone)]
pub struct ModeCatalog {
    shape: Shape,
    modes: Vec<Mode>,
}

impl ModeCatalog {
    /// Square modes with `n <= m <= 16`, sorted by `n^2 + m^2` (ties: smaller n).
    pub fn square(edge: Edge, symmetry: Symmetry) -> Self {
        let first = u8::from(edge == Edge::Fixed);
        let mut pairs = Vec::with_capacity(153);
        for n in first..=16 {
            for m in n..=16 {
                if symmetry == Symmetry::Sym || n != m {
                    pairs.push((n, m));
                }
            }
        }
        pairs.sort_unstable_by_key(|&(n, m)| (u16::from(n).pow(2) + u16::from(m).pow(2), n, m));
        let modes = pairs
            .into_iter()
            .enumerate()
            .map(|(index, (n, m))| {
                let resolved = if symmetry == Symmetry::Mixed {
                    // Mixed uses the off-diagonal basis: diagonal anti modes are
                    // identically zero. The symmetric catalog retains diagonals.
                    if index % 2 == 0 {
                        Symmetry::Anti
                    } else {
                        Symmetry::Sym
                    }
                } else {
                    symmetry
                };
                match SquareMode::new(n, m, edge, resolved) {
                    Ok(mode) => Mode::Square(mode),
                    // The loops enforce valid orders and resolve every symmetry.
                    Err(_) => unreachable!("the catalog enumerates valid square modes"),
                }
            })
            .collect();
        // Swapping n and m only changes the sign of an anti mode (or nothing
        // for a symmetric mode), so both permutations have the same |f| field.
        Self {
            shape: Shape::Square,
            modes,
        }
    }

    /// Drum modes with `n <= 16` and `k <= 8`, sorted by the zero `j_{n,k}`,
    /// without the hollow "whispering gallery" modes (see
    /// [`CircleMode::is_hollow`]). Which modes are hollow is a property of the
    /// mode alone, so the catalog never depends on terminal size.
    pub fn circle() -> Self {
        let mut modes: Vec<CircleMode> = Vec::with_capacity(137);
        for n in 0..=MAX_ORDER {
            for k in 1..=MAX_ZEROS {
                match CircleMode::new(n, k) {
                    Ok(mode) => modes.push(mode),
                    Err(_) => unreachable!("the catalog enumerates valid drum modes"),
                }
            }
        }
        modes.retain(|mode| !mode.is_hollow());
        modes.sort_by(|a, b| a.zero().total_cmp(&b.zero()).then(a.n().cmp(&b.n())));
        Self {
            shape: Shape::Circle,
            modes: modes.into_iter().map(Mode::Circle).collect(),
        }
    }

    pub fn for_plate(config: &PlateConfig) -> Self {
        match config.shape {
            Shape::Square => Self::square(config.edge, config.symmetry),
            Shape::Circle => Self::circle(),
        }
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    pub fn modes(&self) -> &[Mode] {
        &self.modes
    }

    pub fn len(&self) -> usize {
        self.modes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modes.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<Mode> {
        self.modes.get(index).copied()
    }

    /// Reachability filters the full catalog; it never renumbers its entries.
    pub fn reachable_modes(
        &self,
        max_order: MaxOrder,
        plate_width: usize,
        plate_height: usize,
    ) -> impl Iterator<Item = (usize, Mode)> + '_ {
        let limit = order_limit(max_order, plate_width, plate_height);
        self.modes
            .iter()
            .copied()
            .enumerate()
            .filter(move |&(_, mode)| mode.order() <= limit)
    }
}

/// Keep approximately six sub-pixels between nodal lines, within orders 4..16.
pub fn order_limit(max_order: MaxOrder, plate_width: usize, plate_height: usize) -> u8 {
    match max_order {
        MaxOrder::Auto => (plate_width.min(plate_height) / 6).clamp(4, 16) as u8,
        MaxOrder::Fixed(order) => order.clamp(4, 16),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eigenvalue(mode: Mode) -> u16 {
        u16::from(mode.first()).pow(2) + u16::from(mode.second()).pow(2)
    }

    #[test]
    fn full_catalog_is_sorted_and_uses_a_stable_tie_order() {
        let catalog = ModeCatalog::square(Edge::Free, Symmetry::Anti);
        assert_eq!(catalog.modes().len(), 136);
        assert_eq!(
            catalog.get(0).map(|mode| (mode.first(), mode.second())),
            Some((0, 1))
        );
        assert!(catalog.modes().windows(2).all(|pair| {
            (eigenvalue(pair[0]), pair[0].first(), pair[0].second())
                < (eigenvalue(pair[1]), pair[1].first(), pair[1].second())
        }));
        let tied: Vec<_> = catalog
            .modes()
            .iter()
            .filter(|&&mode| eigenvalue(mode) == 25)
            .map(|mode| (mode.first(), mode.second()))
            .collect();
        assert_eq!(tied, [(0, 5), (3, 4)]);
    }

    #[test]
    fn resize_changes_reachability_without_changing_catalog_indices() {
        let catalog = ModeCatalog::square(Edge::Free, Symmetry::Anti);
        let small: Vec<_> = catalog.reachable_modes(MaxOrder::Auto, 24, 40).collect();
        let large: Vec<_> = catalog.reachable_modes(MaxOrder::Auto, 200, 200).collect();
        assert!(small.len() < large.len());
        for (index, mode) in small {
            assert_eq!(large[index], (index, mode));
            assert!(mode.order() <= 4);
        }
        assert_eq!(order_limit(MaxOrder::Auto, 1, 1), 4);
        assert_eq!(order_limit(MaxOrder::Auto, 96, 300), 16);
        assert_eq!(order_limit(MaxOrder::Fixed(8), 1, 1), 8);
    }

    #[test]
    fn fixed_and_symmetric_catalogs_keep_their_valid_modes() {
        let fixed = ModeCatalog::square(Edge::Fixed, Symmetry::Anti);
        assert_eq!(fixed.modes().len(), 120);
        assert!(fixed.modes().iter().all(|mode| mode.first() >= 1));
        let sym = ModeCatalog::square(Edge::Free, Symmetry::Sym);
        assert_eq!(sym.modes().len(), 153);
        assert_eq!(
            sym.get(0).map(|mode| (mode.first(), mode.second())),
            Some((0, 0))
        );
    }

    #[test]
    fn mixed_catalog_alternates_nondegenerate_off_diagonal_pairs() {
        let mixed = ModeCatalog::square(Edge::Free, Symmetry::Mixed);
        assert_eq!(mixed.modes().len(), 136);
        for (index, mode) in mixed.modes().iter().enumerate() {
            let Mode::Square(mode) = mode else {
                panic!("a square catalog holds square modes");
            };
            assert_ne!(mode.n(), mode.m());
            assert_eq!(
                mode.symmetry(),
                if index % 2 == 0 {
                    Symmetry::Anti
                } else {
                    Symmetry::Sym
                }
            );
        }
    }

    #[test]
    fn drum_catalog_is_sorted_by_zero_and_starts_with_the_known_modes() {
        let catalog = ModeCatalog::circle();
        // 17 orders by 8 zeros, minus the hollow whispering-gallery modes.
        assert!((120..=126).contains(&catalog.len()), "{}", catalog.len());
        assert!(
            catalog
                .modes()
                .iter()
                .all(|&mode| matches!(mode, Mode::Circle(m) if !m.is_hollow()))
        );
        let labels: Vec<_> = catalog
            .modes()
            .iter()
            .take(6)
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            labels,
            ["J(0,1)", "J(1,1)", "J(2,1)", "J(0,2)", "J(3,1)", "J(1,2)"]
        );
        assert!(catalog.modes().windows(2).all(|pair| {
            let zero = |mode: Mode| match mode {
                Mode::Circle(mode) => mode.zero(),
                Mode::Square(_) => unreachable!(),
            };
            zero(pair[0]) < zero(pair[1])
        }));
    }

    #[test]
    fn drum_reachability_never_renumbers_and_grows_with_size() {
        let catalog = ModeCatalog::circle();
        let small = catalog.reachable_modes(MaxOrder::Auto, 24, 24).count();
        let large = catalog.reachable_modes(MaxOrder::Auto, 400, 400).count();
        // A tiny drum can only show its four simplest figures.
        assert_eq!(small, 4);
        // A large one reaches five full tiers of twelve notes.
        assert!(large >= 60, "{large}");
        for (index, mode) in catalog.reachable_modes(MaxOrder::Fixed(8), 400, 400) {
            assert_eq!(catalog.get(index), Some(mode));
            assert!(mode.order() <= 8);
        }
    }

    #[test]
    fn plate_configuration_selects_the_catalog() {
        let mut config = PlateConfig::default();
        assert_eq!(ModeCatalog::for_plate(&config).shape(), Shape::Square);
        config.shape = Shape::Circle;
        let catalog = ModeCatalog::for_plate(&config);
        assert_eq!(catalog.shape(), Shape::Circle);
        assert!(
            catalog
                .modes()
                .iter()
                .all(|mode| mode.shape() == Shape::Circle)
        );
    }
}
