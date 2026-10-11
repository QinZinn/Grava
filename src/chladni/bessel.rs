//! Positive zeros of Bessel functions of the first kind, `J_n`.
//!
//! Drum-head modes need `j_{n,k}`, the k-th positive zero of `J_n`. The table
//! is built once, on first use, by scanning `libm::jn` for sign changes and
//! refining each bracket by bisection. It needs no data files and never
//! depends on terminal size.

use std::sync::OnceLock;

/// The largest Bessel order in the table.
pub const MAX_ORDER: u8 = 16;
/// The largest zero index in the table; zeros are numbered from 1.
pub const MAX_ZEROS: u8 = 8;

const SCAN_STEP: f64 = 0.05;
/// Safety bound for the scan; j_{16,8} is about 52.
const SCAN_LIMIT: f64 = 200.0;
const BISECTION_STEPS: usize = 80;

type Table = [[f64; MAX_ZEROS as usize]; MAX_ORDER as usize + 1];

/// The k-th positive zero of `J_n`, for `n <= 16` and `1 <= k <= 8`.
pub fn zero(n: u8, k: u8) -> Option<f64> {
    if n > MAX_ORDER || k == 0 || k > MAX_ZEROS {
        return None;
    }
    Some(table()[usize::from(n)][usize::from(k) - 1])
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [[0.0; MAX_ZEROS as usize]; MAX_ORDER as usize + 1];
        for (n, row) in table.iter_mut().enumerate() {
            find_zeros(n as i32, row);
        }
        table
    })
}

/// Fill `zeros` with the first `zeros.len()` positive zeros of `J_n`.
///
/// `J_n(x)` is positive on `(0, j_{n,1})` and `j_{n,1} > n`, so scanning can
/// start at `max(n, 0.1)`. Consecutive zeros are about pi apart, which is far
/// wider than the scan step, so no sign change can be missed.
fn find_zeros(n: i32, zeros: &mut [f64]) {
    let mut found = 0;
    let mut previous_x = f64::from(n).max(0.1);
    let mut previous = libm::jn(n, previous_x);
    let mut x = previous_x;
    while found < zeros.len() && x < SCAN_LIMIT {
        x += SCAN_STEP;
        let value = libm::jn(n, x);
        if (previous > 0.0) != (value > 0.0) {
            zeros[found] = bisect(n, previous_x, x, previous > 0.0);
            found += 1;
        }
        previous_x = x;
        previous = value;
    }
    debug_assert_eq!(found, zeros.len(), "scan limit too small for J_{n}");
}

fn bisect(n: i32, mut low: f64, mut high: f64, low_positive: bool) -> f64 {
    for _ in 0..BISECTION_STEPS {
        let middle = 0.5 * (low + high);
        if (libm::jn(n, middle) > 0.0) == low_positive {
            low = middle;
        } else {
            high = middle;
        }
    }
    0.5 * (low + high)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn classic_zeros_match_tabulated_values() {
        for (n, k, expected) in [
            (0, 1, 2.404_826),
            (0, 2, 5.520_078),
            (1, 1, 3.831_706),
            (2, 1, 5.135_622),
        ] {
            let actual = zero(n, k).unwrap();
            assert!((actual - expected).abs() < 1e-5, "j_{n},{k} = {actual}");
        }
    }

    #[test]
    fn every_entry_is_a_root_and_zeros_increase_with_k_and_n() {
        for n in 0..=MAX_ORDER {
            let mut previous = 0.0;
            for k in 1..=MAX_ZEROS {
                let value = zero(n, k).unwrap();
                assert!(value > previous && value.is_finite());
                assert!(libm::jn(i32::from(n), value).abs() < 1e-9, "J_{n}({value})");
                previous = value;
            }
            if n > 0 {
                // Zeros of neighbouring orders interlace: j_{n-1,1} < j_{n,1}.
                assert!(zero(n - 1, 1).unwrap() < zero(n, 1).unwrap());
            }
        }
    }

    #[test]
    fn out_of_table_requests_are_rejected() {
        assert_eq!(zero(17, 1), None);
        assert_eq!(zero(0, 0), None);
        assert_eq!(zero(0, 9), None);
        // The highest tabulated zero (J_16, k = 8) lies beyond 45.
        assert!(zero(16, 8).unwrap() > 45.0);
    }
}
