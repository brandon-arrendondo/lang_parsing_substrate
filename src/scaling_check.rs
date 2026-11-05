//! Timing support for the tests that check a scanner's cost grows linearly.
//!
//! A test compares the cost of a small input with the cost of one four
//! times its size, so the answer doesn't depend on how fast the machine is.
//! Linear cost gives a ratio near 4 and quadratic cost near 16, and the test
//! asserts the ratio stays under 8.
//!
//! Load on the machine can still slow one timing and skew the ratio, so:
//!
//! - the small and large scans alternate, round by round, so a burst of
//!   load lands on both sizes rather than on one;
//! - each size keeps its fastest round, since load only ever adds time;
//! - a ratio over the bound is measured again before it counts as a
//!   failure. Noise rarely repeats, but quadratic cost fails every attempt.

use std::time::{Duration, Instant};

/// Rounds per attempt; each times one small and one large scan.
const ROUNDS: usize = 5;

/// Attempts before a ratio over the bound is reported.
const ATTEMPTS: usize = 3;

/// The ratio a linear scan stays under when the input grows four times.
pub(crate) const LINEAR_BOUND: u32 = 8;

/// Whether `scan(large)` takes less than `bound` times as long as
/// `scan(small)`, following the procedure in the module documentation.
pub(crate) fn costs_under(small: &str, large: &str, bound: u32, scan: impl Fn(&str)) -> bool {
    (0..ATTEMPTS).any(|_| {
        let (small_time, large_time) = fastest_of_interleaved(small, large, &scan);
        large_time < small_time * bound
    })
}

/// The fastest time of each of `small` and `large` over [`ROUNDS`]
/// alternating rounds.
fn fastest_of_interleaved(small: &str, large: &str, scan: &impl Fn(&str)) -> (Duration, Duration) {
    let time = |source: &str| {
        let start = Instant::now();
        scan(source);
        start.elapsed()
    };
    (0..ROUNDS).fold((Duration::MAX, Duration::MAX), |(s, l), _| {
        (s.min(time(small)), l.min(time(large)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cost proportional to the square of the input's length.
    fn quadratic(source: &str) {
        let n = source.len();
        let mut acc = 0u64;
        for i in 0..n {
            for j in 0..n {
                acc = acc.wrapping_add((i ^ j) as u64);
            }
        }
        std::hint::black_box(acc);
    }

    /// Cost proportional to the input's length.
    fn linear(source: &str) {
        let mut acc = 0u64;
        for _ in 0..64 {
            for b in source.bytes() {
                acc = acc.wrapping_add(b as u64);
            }
        }
        std::hint::black_box(acc);
    }

    /// Growing the input eight times gives a quadratic scan a ratio near
    /// 64, so even a run slowed by load stays well over the bound.
    #[test]
    fn a_quadratic_scan_is_over_the_linear_bound() {
        let small = "x".repeat(500);
        let large = "x".repeat(4_000);
        assert!(!costs_under(&small, &large, LINEAR_BOUND, quadratic));
    }

    #[test]
    fn a_linear_scan_is_under_the_linear_bound() {
        let small = "x".repeat(250_000);
        let large = "x".repeat(1_000_000);
        assert!(costs_under(&small, &large, LINEAR_BOUND, linear));
    }
}
