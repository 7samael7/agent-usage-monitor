//! The boundary between stored integers and in-process decimals.
//!
//! Money is a `Decimal` everywhere it is computed and an integer everywhere it
//! is stored. A float never appears on either side, and this module is the only
//! place the two representations meet — so if the scale is ever wrong, it is
//! wrong in one function with its own tests rather than scattered across every
//! query.

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive as _;

/// Nano-units per whole unit. `$1.00` is `1_000_000_000`.
pub const NANO: i64 = 1_000_000_000;

/// Stored integer to decimal. Always exact.
///
/// Normalised, because the scale is an artefact of storage rather than a
/// statement about the rate. `Decimal::new(5_000_000_000, 9)` is `5.000000000`,
/// and those nine zeros travel all the way to the screen as `$5.000000000 / in`
/// — which reads as false precision about a figure that is simply five dollars.
/// Normalising changes no value, only how many digits claim to be significant.
#[must_use]
pub fn to_decimal(nano: i64) -> Decimal {
    Decimal::new(nano, 9).normalize()
}

/// Decimal to stored integer.
///
/// `None` when the value will not fit, which for a per-million-token rate means
/// something on the order of nine billion dollars and is certainly a typo — one
/// worth rejecting rather than wrapping into a plausible small number.
///
/// Beyond nine decimal places the value is rounded, which is a billionth of a
/// dollar per million tokens: below the resolution of anything a provider
/// publishes.
#[must_use]
pub fn from_decimal(value: Decimal) -> Option<i64> {
    value
        .checked_mul(Decimal::from(NANO))
        .map(|scaled| scaled.round())
        .and_then(|scaled| scaled.to_i64())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::str::FromStr as _;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn the_scale_is_what_the_schema_says_it_is() {
        // $15.00 per million tokens is 15_000_000_000 nano-USD.
        assert_eq!(from_decimal(d("15.00")), Some(15_000_000_000));
        assert_eq!(to_decimal(15_000_000_000), d("15"));
    }

    #[test]
    fn published_rates_round_trip_without_drift() {
        // Every published rate shape: whole, half, and the sub-dollar cache
        // read rates where a float would start to visibly lie.
        for s in [
            "15.00",
            "1.50",
            "0.30",
            "0.075",
            "18.75",
            "3.00",
            "0.0000001",
        ] {
            let nano = from_decimal(d(s)).unwrap();
            assert_eq!(to_decimal(nano), d(s), "{s} did not survive the round trip");
        }
    }

    #[test]
    fn a_rate_too_large_to_store_is_refused_rather_than_wrapped() {
        // i64 nano tops out around nine billion dollars. Wrapping would turn a
        // typo into a small, plausible, entirely wrong rate.
        assert_eq!(from_decimal(d("100000000000")), None);
    }

    #[test]
    fn a_stored_rate_does_not_come_back_claiming_nine_decimal_places() {
        // $5.00 stored is 5_000_000_000 nano. Without normalising it returns as
        // "5.000000000", which reaches the screen verbatim and reads as a
        // measured-to-the-nanodollar rate rather than as five dollars.
        assert_eq!(to_decimal(5_000_000_000).to_string(), "5");
        assert_eq!(to_decimal(750_000_000).to_string(), "0.75");
        assert_eq!(to_decimal(75_000_000).to_string(), "0.075");
    }

    #[test]
    fn zero_is_a_real_rate_and_stays_one() {
        // Free tiers exist. Zero must not be confused with absent, which is why
        // absence is Option and never 0.
        assert_eq!(from_decimal(Decimal::ZERO), Some(0));
        assert_eq!(to_decimal(0), Decimal::ZERO);
    }
}
