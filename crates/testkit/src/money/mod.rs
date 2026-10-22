//! Adversarial value generation and conservation checking for money math —
//! the arithmetic bugs hand-written tests tend to miss.
//!
//! # Signed amounts
//!
//! This module follows the Stellar convention that a token **amount** is a
//! signed [`i128`]. A negative amount is a real value — a debit, a refund, or
//! the result of a subtraction — not an error to be filtered out. The amount
//! generators and the [`Conservation`] snapshot all use signed `i128` for
//! amounts, and the generators deliberately include the signed extremes
//! (`-1`, `i128::MIN`) so negative-balance and refund paths are exercised.
//!
//! Basis-point *fractions* ([`bps_values`]) are the one unsigned case: a
//! fraction is a [`u32`] (`10_000` bps = 100%) and cannot be negative.

mod conversions;
mod generators;
mod invariants;
mod rounding;

pub use conversions::{
    assert_in_range, assert_roundtrip, from_stroops, stroops_to_decimal_string, to_stroops,
    to_stroops_with_decimals, STELLAR_DECIMALS, STELLAR_SCALE,
};
pub use generators::{adversarial_amounts, amounts_in, bps_values, overflow_edge_triples};
pub use invariants::{assert_conserved_over, Conservation};
pub use rounding::{assert_rounding, assert_split_conserved, RoundingPolicy};
