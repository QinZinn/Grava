//! Spectral peak picking with sub-bin frequency interpolation.

/// The peaks reported in `AudioFeatures::peaks`; only spectrum mapping uses them.
pub const MAX_PEAKS: usize = 8;
/// The strongest peaks of a frame that are folded into the chroma.
pub const MAX_CHROMA_PEAKS: usize = 32;

/// A spectral peak: interpolated frequency and amplitude (a sine of amplitude
/// `a` reports about `a`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Peak {
    pub hz: f32,
    pub magnitude: f32,
}

/// Call `visit` for every local maximum in bins `first..=last` that reaches
/// `floor`. The scan reads one bin beyond each end, so `first >= 1` and
/// `last + 1 < magnitudes.len()` are required; other ranges yield no peaks.
pub fn scan(
    magnitudes: &[f32],
    first: usize,
    last: usize,
    bin_hz: f32,
    floor: f32,
    mut visit: impl FnMut(Peak),
) {
    if first == 0 || last < first || last + 1 >= magnitudes.len() {
        return;
    }
    for bin in first..=last {
        let value = magnitudes[bin];
        if value >= floor && value > magnitudes[bin - 1] && value >= magnitudes[bin + 1] {
            visit(interpolate(magnitudes, bin, bin_hz));
        }
    }
}

/// Parabolic interpolation on log magnitudes around the local maximum `bin`.
/// The window's main lobe is close to a parabola in the log domain, so the
/// frequency error stays far below one bin.
fn interpolate(magnitudes: &[f32], bin: usize, bin_hz: f32) -> Peak {
    const TINY: f32 = 1e-12;
    let left = magnitudes[bin - 1].max(TINY).ln();
    let middle = magnitudes[bin].max(TINY).ln();
    let right = magnitudes[bin + 1].max(TINY).ln();
    let curvature = left - 2.0 * middle + right;
    let offset = if curvature < -1e-9 {
        (0.5 * (left - right) / curvature).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    Peak {
        hz: (bin as f32 + offset) * bin_hz,
        magnitude: (middle - 0.25 * (left - right) * offset).exp(),
    }
}

/// The `N` strongest peaks of one frame, kept sorted by decreasing magnitude.
#[derive(Debug, Clone, Copy)]
pub struct TopPeaks<const N: usize> {
    items: [Peak; N],
    len: usize,
}

impl<const N: usize> Default for TopPeaks<N> {
    fn default() -> Self {
        Self {
            items: [Peak {
                hz: 0.0,
                magnitude: 0.0,
            }; N],
            len: 0,
        }
    }
}

impl<const N: usize> TopPeaks<N> {
    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn as_slice(&self) -> &[Peak] {
        &self.items[..self.len]
    }

    pub fn insert(&mut self, peak: Peak) {
        let mut index = if self.len < N {
            self.len += 1;
            self.len - 1
        } else if N > 0 && peak.magnitude > self.items[N - 1].magnitude {
            N - 1
        } else {
            return;
        };
        while index > 0 && self.items[index - 1].magnitude < peak.magnitude {
            self.items[index] = self.items[index - 1];
            index -= 1;
        }
        self.items[index] = peak;
    }

    /// Drop peaks weaker than `fraction` of the strongest one.
    pub fn retain_relative(&mut self, fraction: f32) {
        if let Some(strongest) = self.as_slice().first() {
            let threshold = strongest.magnitude * fraction;
            while self.len > 1 && self.items[self.len - 1].magnitude < threshold {
                self.len -= 1;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A Hann-windowed spectrum of one sine, computed directly from the
    /// window's transform so the test does not depend on the FFT crate.
    fn hann_line(bins: usize, frequency_bins: f32, amplitude: f32) -> Vec<f32> {
        let kernel = |delta: f32| {
            // Normalized Hann transform: 1 at 0, 0.5 at +-1, 0 at +-2, 3, ...
            let x = std::f32::consts::PI * delta;
            if delta.abs() < 1e-6 {
                1.0
            } else if (delta.abs() - 1.0).abs() < 1e-6 {
                0.5
            } else {
                x.sin() / (x * (1.0 - delta * delta))
            }
        };
        (0..bins)
            .map(|bin| amplitude * kernel(bin as f32 - frequency_bins).abs())
            .collect()
    }

    #[test]
    fn interpolation_recovers_fractional_bins_and_amplitude() {
        for offset in [0.0_f32, 0.2, -0.3, 0.45] {
            let spectrum = hann_line(64, 20.0 + offset, 0.5);
            let mut found = Vec::new();
            scan(&spectrum, 1, 62, 10.0, 1e-4, |peak| found.push(peak));
            let strongest = found
                .iter()
                .max_by(|a, b| a.magnitude.total_cmp(&b.magnitude))
                .unwrap();
            assert!(
                (strongest.hz - (20.0 + offset) * 10.0).abs() < 0.8,
                "offset {offset}: {}",
                strongest.hz
            );
            assert!(
                (strongest.magnitude - 0.5).abs() < 0.04,
                "{}",
                strongest.magnitude
            );
        }
    }

    #[test]
    fn scan_respects_range_floor_and_edges() {
        let spectrum = [0.0, 0.2, 0.0, 0.5, 0.1, 0.9];
        let mut bins = Vec::new();
        scan(&spectrum, 1, 4, 1.0, 0.0, |peak| {
            bins.push(peak.hz.round() as usize)
        });
        assert_eq!(bins, [1, 3]);
        let mut loud = 0;
        scan(&spectrum, 1, 4, 1.0, 0.3, |_| loud += 1);
        assert_eq!(loud, 1);
        // Out-of-range requests return silently instead of indexing.
        for (first, last) in [(0, 3), (3, 2), (1, 5), (1, 99)] {
            scan(&spectrum, first, last, 1.0, 0.0, |_| {
                panic!("no peaks expected")
            });
        }
        // A plateau is one peak, not two.
        let mut plateau = 0;
        scan(&[0.0, 1.0, 1.0, 0.0], 1, 2, 1.0, 0.0, |_| plateau += 1);
        assert_eq!(plateau, 1);
    }

    #[test]
    fn top_peaks_keep_the_strongest_in_order() {
        let mut top = TopPeaks::<8>::default();
        for (index, magnitude) in [0.3, 0.9, 0.1, 0.5, 0.7, 0.2, 0.8, 0.6, 0.4, 1.0, 0.05]
            .into_iter()
            .enumerate()
        {
            top.insert(Peak {
                hz: index as f32,
                magnitude,
            });
        }
        let magnitudes: Vec<_> = top.as_slice().iter().map(|peak| peak.magnitude).collect();
        assert_eq!(magnitudes, [1.0, 0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3]);
        top.retain_relative(0.55);
        assert_eq!(top.as_slice().len(), 5);
        top.clear();
        assert!(top.as_slice().is_empty());
        top.retain_relative(0.5);
    }
}
