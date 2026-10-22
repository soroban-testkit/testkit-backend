//! Reusable rounding policy assertions for money math.
//!
//! When contracts perform division (e.g., splitting a payment), the rounding
//! direction matters: rounding down favors the contract, rounding up favors
//! the user, and truncation toward zero is symmetric. These assertions
//! verify that a division rounds in the documented direction.

use crate::core::TestkitError;

/// The direction a division rounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundingPolicy {
    /// Truncate toward zero (e.g., `3 / 2 = 1`, `-3 / 2 = -1`).
    Truncate,
    /// Round toward positive infinity (e.g., `3 / 2 = 2`, `-3 / 2 = -1`).
    Floor,
    /// Round toward negative infinity (e.g., `3 / 2 = 1`, `-3 / 2 = -2`).
    Ceil,
    /// Round half away from zero (e.g., `3 / 2 = 2`, `5 / 2 = 3`).
    HalfAwayFromZero,
}

impl RoundingPolicy {
    /// Compute `a / b` using this rounding policy.
    ///
    /// Returns `None` if `b` is zero.
    pub fn apply(self, a: i128, b: i128) -> Option<i128> {
        if b == 0 {
            return None;
        }
        match self {
            RoundingPolicy::Truncate => Some(a / b),
            RoundingPolicy::Floor => Some(a.div_euclid(b)),
            RoundingPolicy::Ceil => {
                let q = a.div_euclid(b);
                if a.rem_euclid(b) != 0 {
                    Some(q + 1)
                } else {
                    Some(q)
                }
            }
            RoundingPolicy::HalfAwayFromZero => {
                let q = a / b;
                let r = a % b;
                let abs_r = r.unsigned_abs();
                let abs_b = b.unsigned_abs();
                if abs_r * 2 >= abs_b {
                    if (a > 0) == (b > 0) {
                        Some(q + 1)
                    } else {
                        Some(q - 1)
                    }
                } else {
                    Some(q)
                }
            }
        }
    }

    /// Human-readable name for diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            RoundingPolicy::Truncate => "truncate toward zero",
            RoundingPolicy::Floor => "floor (toward negative infinity)",
            RoundingPolicy::Ceil => "ceil (toward positive infinity)",
            RoundingPolicy::HalfAwayFromZero => "half away from zero",
        }
    }
}

/// Assert that `actual` equals the expected result of dividing `a / b`
/// under the given [`RoundingPolicy`].
///
/// Panics with a [`TestkitError::AssertionFailed`] showing the operands,
/// expected result, and actual result if the assertion fails.
///
/// # Example
///
/// ```
/// use soroban_testkit::money::{assert_rounding, RoundingPolicy};
///
/// assert_rounding(3, 2, 1, RoundingPolicy::Truncate);
/// assert_rounding(3, 2, 2, RoundingPolicy::Ceil);
/// ```
pub fn assert_rounding(a: i128, b: i128, actual: i128, policy: RoundingPolicy) {
    let expected = policy.apply(a, b).unwrap_or_else(|| {
        panic!(
            "{}",
            TestkitError::AssertionFailed(format!(
                "assert_rounding({a}, {b}): divisor is zero"
            ))
        )
    });
    if actual != expected {
        panic!(
            "{}",
            TestkitError::AssertionFailed(format!(
                "rounding mismatch: {a} / {b} = {actual} but {} expects {expected}",
                policy.name()
            ))
        );
    }
}

/// Assert that the sum of `parts` equals `total`, allowing at most
/// `tolerance` of rounding dust per part.
///
/// This is the standard check for "split the pot among N recipients":
/// the parts must sum to within `len(parts) * tolerance` of the total.
///
/// # Example
///
/// ```
/// use soroban_testkit::money::assert_split_conserved;
///
/// // 10 split among 3: 3 + 3 + 4 = 10 (no dust)
/// assert_split_conserved(10, &[3, 3, 4], 0);
/// ```
pub fn assert_split_conserved(total: i128, parts: &[i128], tolerance_per_part: i128) {
    let sum: i128 = parts.iter().fold(0, |acc, p| acc.saturating_add(*p));
    let max_dust = tolerance_per_part.saturating_mul(parts.len() as i128);
    let delta = (total - sum).abs();
    if delta > max_dust {
        panic!(
            "{}",
            TestkitError::AssertionFailed(format!(
                "split not conserved: total={total} sum_of_parts={sum} \
                 delta={delta} (max allowed dust={max_dust}, {} parts × {tolerance_per_part})",
                parts.len()
            ))
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_floors_toward_zero() {
        assert_eq!(RoundingPolicy::Truncate.apply(3, 2), Some(1));
        assert_eq!(RoundingPolicy::Truncate.apply(-3, 2), Some(-1));
        assert_eq!(RoundingPolicy::Truncate.apply(7, 3), Some(2));
    }

    #[test]
    fn floor_rounds_down() {
        assert_eq!(RoundingPolicy::Floor.apply(3, 2), Some(1));
        assert_eq!(RoundingPolicy::Floor.apply(-3, 2), Some(-2));
        assert_eq!(RoundingPolicy::Floor.apply(7, 3), Some(2));
    }

    #[test]
    fn ceil_rounds_up() {
        assert_eq!(RoundingPolicy::Ceil.apply(3, 2), Some(2));
        assert_eq!(RoundingPolicy::Ceil.apply(-3, 2), Some(-1));
        assert_eq!(RoundingPolicy::Ceil.apply(6, 3), Some(2));
    }

    #[test]
    fn half_away_from_zero_rounds_correctly() {
        assert_eq!(RoundingPolicy::HalfAwayFromZero.apply(3, 2), Some(2));
        assert_eq!(RoundingPolicy::HalfAwayFromZero.apply(-3, 2), Some(-2));
        assert_eq!(RoundingPolicy::HalfAwayFromZero.apply(4, 2), Some(2));
    }

    #[test]
    fn zero_divisor_returns_none() {
        assert_eq!(RoundingPolicy::Truncate.apply(5, 0), None);
    }

    #[test]
    fn assert_rounding_passes_correct_value() {
        assert_rounding(3, 2, 1, RoundingPolicy::Truncate);
    }

    #[test]
    #[should_panic(expected = "rounding mismatch")]
    fn assert_rounding_panics_on_wrong_value() {
        assert_rounding(3, 2, 2, RoundingPolicy::Truncate);
    }

    #[test]
    fn assert_split_conserved_passes_exact_split() {
        assert_split_conserved(10, &[3, 3, 4], 0);
    }

    #[test]
    fn assert_split_conserved_passes_within_tolerance() {
        assert_split_conserved(10, &[3, 3, 3], 1); // 9, dust=1 per part
    }

    #[test]
    #[should_panic(expected = "split not conserved")]
    fn assert_split_conserved_panics_when_leaking() {
        assert_split_conserved(10, &[3, 3, 1], 0); // 7, missing 3
    }
}
