//! Decimal-aware conversion helpers with checked bounds for money math.
//!
//! Stellar token amounts are integer stroops (10^7 = 1 whole unit). These
//! helpers convert between human-readable decimal amounts and stroops with
//! overflow/underflow detection, so tests can assert that contracts handle
//! boundary values correctly.

use crate::core::TestkitError;

/// The standard number of decimal places for Stellar assets (7).
pub const STELLAR_DECIMALS: u32 = 7;

/// The scale factor for Stellar assets (10^7).
pub const STELLAR_SCALE: i128 = 10_i128.pow(STELLAR_DECIMALS);

/// Convert a whole-unit amount to stroops, checking for overflow.
///
/// `whole_units` is in the asset's natural unit (e.g., XLM). The result is
/// `whole_units * 10^7`. Returns `Err(TestkitError::Misuse)` if the
/// multiplication would overflow `i128`.
///
/// # Example
///
/// ```
/// use soroban_testkit::money::to_stroops;
///
/// assert_eq!(to_stroops(1), Ok(10_000_000));
/// assert_eq!(to_stroops(0), Ok(0));
/// assert_eq!(to_stroops(-1), Ok(-10_000_000));
/// ```
pub fn to_stroops(whole_units: i128) -> Result<i128, TestkitError> {
    whole_units.checked_mul(STELLAR_SCALE).ok_or_else(|| {
        TestkitError::Misuse(format!(
            "converting {whole_units} whole units to stroops overflows i128: \
             {whole_units} * {STELLAR_SCALE} does not fit"
        ))
    })
}

/// Convert a decimal amount with arbitrary precision to stroops, checking
/// for overflow.
///
/// `value` is in whole units, `decimals` specifies the fractional precision
/// (e.g., `value = 1_50, decimals = 2` is 1.50). The result rounds toward
/// zero (truncates). Returns `Err(TestkitError::Misuse)` if the scaled
/// multiplication would overflow `i128`.
///
/// # Example
///
/// ```
/// use soroban_testkit::money::to_stroops_with_decimals;
///
/// // 1.50 XLM with 2 decimals → 15,000,000 stroops
/// assert_eq!(to_stroops_with_decimals(150, 2), Ok(15_000_000));
/// // 0.0000001 XLM (1 stroop) with 7 decimals
/// assert_eq!(to_stroops_with_decimals(1, 7), Ok(1));
/// ```
pub fn to_stroops_with_decimals(value: i128, decimals: u32) -> Result<i128, TestkitError> {
    if decimals > STELLAR_DECIMALS {
        // Sub-stroop precision: truncate to whole stroops
        let scale_diff = decimals - STELLAR_DECIMALS;
        let divisor = 10_i128.pow(scale_diff);
        let stroops = value.checked_div(divisor).ok_or_else(|| {
            TestkitError::Misuse(format!(
                "dividing {value} by 10^{scale_diff} failed (decimals={decimals})"
            ))
        })?;
        Ok(stroops)
    } else {
        let scale_diff = STELLAR_DECIMALS - decimals;
        let multiplier = 10_i128.pow(scale_diff);
        value.checked_mul(multiplier).ok_or_else(|| {
            TestkitError::Misuse(format!(
                "converting {value} with {decimals} decimals to stroops overflows i128: \
                 {value} * {multiplier} does not fit"
            ))
        })
    }
}

/// Convert stroops to whole units, returning the integer part and the
/// fractional remainder in stroops.
///
/// # Example
///
/// ```
/// use soroban_testkit::money::from_stroops;
///
/// assert_eq!(from_stroops(15_000_000), (1, 5_000_000));
/// assert_eq!(from_stroops(9_999_999), (0, 9_999_999));
/// ```
pub fn from_stroops(stroops: i128) -> (i128, i128) {
    let whole = stroops.div_euclid(STELLAR_SCALE);
    let remainder = stroops.rem_euclid(STELLAR_SCALE);
    (whole, remainder)
}

/// Convert stroops to a fixed-point decimal string with `decimals` fractional
/// digits, truncating extra precision.
///
/// # Example
///
/// ```
/// use soroban_testkit::money::stroops_to_decimal_string;
///
/// assert_eq!(stroops_to_decimal_string(15_000_000, 2), "1.50");
/// assert_eq!(stroops_to_decimal_string(1, 7), "0.0000001");
/// ```
pub fn stroops_to_decimal_string(stroops: i128, decimals: u32) -> String {
    let sign = if stroops < 0 { "-" } else { "" };
    let abs = stroops.unsigned_abs();
    let scale = 10_u128.pow(decimals);
    let whole = abs / scale;
    let frac = abs % scale;
    format!("{sign}{whole}.{frac:0width$}", width = decimals as usize)
}

/// Assert that `value` is within the inclusive range `[min, max]`.
///
/// Panics with a [`TestkitError::AssertionFailed`] showing the value and
/// bounds if the assertion fails.
///
/// # Example
///
/// ```
/// use soroban_testkit::money::assert_in_range;
///
/// assert_in_range(5, 0, 10);
/// ```
pub fn assert_in_range(value: i128, min: i128, max: i128) {
    if value < min || value > max {
        panic!(
            "{}",
            TestkitError::AssertionFailed(format!(
                "value {value} is outside the expected range [{min}, {max}]"
            ))
        );
    }
}

/// Assert that a conversion does not overflow by checking the result
/// round-trips back to the original (within truncation tolerance).
///
/// # Example
///
/// ```
/// use soroban_testkit::money::assert_roundtrip;
///
/// assert_roundtrip(100, 100, 0);
/// ```
pub fn assert_roundtrip(original: i128, converted: i128, tolerance: i128) {
    let delta = (original - converted).abs();
    if delta > tolerance {
        panic!(
            "{}",
            TestkitError::AssertionFailed(format!(
                "roundtrip mismatch: original={original} converted={converted} \
                 delta={delta} (tolerance={tolerance})"
            ))
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_stroops_basic() {
        assert_eq!(to_stroops(1).unwrap(), 10_000_000);
        assert_eq!(to_stroops(0).unwrap(), 0);
        assert_eq!(to_stroops(-1).unwrap(), -10_000_000);
    }

    #[test]
    fn to_stroops_overflow_is_rejected() {
        assert!(to_stroops(i128::MAX).is_err());
    }

    #[test]
    fn to_stroops_with_decimals_various() {
        assert_eq!(to_stroops_with_decimals(150, 2).unwrap(), 15_000_000);
        assert_eq!(to_stroops_with_decimals(1, 7).unwrap(), 1);
        assert_eq!(to_stroops_with_decimals(100, 0).unwrap(), 1_000_000_000);
    }

    #[test]
    fn to_stroops_with_excess_decimals_truncates() {
        // 0.00000001 (8 decimals) truncates to 0 stroops
        assert_eq!(to_stroops_with_decimals(1, 8).unwrap(), 0);
    }

    #[test]
    fn from_stroops_splits_correctly() {
        assert_eq!(from_stroops(15_000_000), (1, 5_000_000));
        assert_eq!(from_stroops(0), (0, 0));
        assert_eq!(from_stroops(-15_000_000), (-1, 5_000_000));
    }

    #[test]
    fn stroops_to_decimal_string_formats() {
        assert_eq!(stroops_to_decimal_string(15_000_000, 2), "1.50");
        assert_eq!(stroops_to_decimal_string(1, 7), "0.0000001");
        assert_eq!(stroops_to_decimal_string(0, 2), "0.00");
    }

    #[test]
    fn assert_in_range_passes_within_bounds() {
        assert_in_range(5, 0, 10);
        assert_in_range(0, 0, 10);
        assert_in_range(10, 0, 10);
    }

    #[test]
    #[should_panic(expected = "outside the expected range")]
    fn assert_in_range_panics_outside_bounds() {
        assert_in_range(11, 0, 10);
    }

    #[test]
    fn assert_roundtrip_passes_with_tolerance() {
        assert_roundtrip(100, 100, 0);
        assert_roundtrip(100, 99, 1);
    }

    #[test]
    #[should_panic(expected = "roundtrip mismatch")]
    fn assert_roundtrip_panics_beyond_tolerance() {
        assert_roundtrip(100, 50, 1);
    }
}
