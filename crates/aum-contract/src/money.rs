//! Money on the wire.
//!
//! Money is **always** serialized as a decimal string, never as a JSON number.
//! A JSON number is an IEEE-754 double in every JavaScript consumer; summed
//! token costs (frequently in the 1e-7 range, summed over thousands of requests)
//! lose precision silently. A string cannot.

use std::fmt;
use std::str::FromStr;

use rust_decimal::Decimal;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An exact monetary amount in a given currency.
///
/// Constructed only from a [`Decimal`]; there is deliberately no `From<f64>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Money(Decimal);

impl Money {
    pub const ZERO: Self = Self(Decimal::ZERO);

    #[must_use]
    pub const fn new(amount: Decimal) -> Self {
        Self(amount)
    }

    #[must_use]
    pub const fn amount(self) -> Decimal {
        self.0
    }

    /// Storage form: integer nano-units. `i64` covers ±9.2 billion units exactly.
    ///
    /// Returns `None` if the value does not fit, rather than saturating into a
    /// wrong-but-plausible number.
    #[must_use]
    pub fn to_nanos(self) -> Option<i64> {
        use rust_decimal::prelude::ToPrimitive;
        (self.0 * Decimal::from(1_000_000_000i64)).round().to_i64()
    }

    #[must_use]
    pub fn from_nanos(nanos: i64) -> Self {
        Self(Decimal::new(nanos, 9))
    }

    /// Exact addition. Costs are summed exactly and rounded **once**, at display.
    /// Rounding per-request and then summing drifts the total and forfeits any
    /// claim that it is exact.
    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }
}

impl std::ops::Add for Money {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl std::iter::Sum for Money {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, |a, b| a + b)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for Money {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Money {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Money;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a decimal amount encoded as a string, e.g. \"0.004125\"")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Money, E> {
                Decimal::from_str(v).map(Money).map_err(E::custom)
            }
        }
        d.deserialize_str(V)
    }
}

/// Display currency. USD is the canonical pricing currency; EUR and CZK are
/// presentation, produced by applying a dated exchange rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    Usd,
    Eur,
    Czk,
}

impl Currency {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Usd => "USD",
            Self::Eur => "EUR",
            Self::Czk => "CZK",
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn serializes_as_string_not_number() {
        let m = Money::new(Decimal::from_str("0.004125").unwrap());
        assert_eq!(serde_json::to_string(&m).unwrap(), "\"0.004125\"");
    }

    #[test]
    fn round_trips_without_loss() {
        // A value that f64 cannot represent exactly.
        let original = Money::new(Decimal::from_str("0.1088527500000001").unwrap());
        let json = serde_json::to_string(&original).unwrap();
        let back: Money = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn rejects_a_json_number() {
        // Guards the wire contract: a producer that emits a bare number is a bug,
        // and must fail loudly here rather than be silently coerced.
        assert!(serde_json::from_str::<Money>("0.004125").is_err());
    }

    #[test]
    fn nanos_round_trip() {
        let m = Money::new(Decimal::from_str("15.000000001").unwrap());
        assert_eq!(Money::from_nanos(m.to_nanos().unwrap()), m);
    }

    #[test]
    fn sum_is_exact_over_many_small_amounts() {
        // 10_000 x 0.0000001 == 0.001 exactly. In f64 this accumulates error.
        let one = Money::new(Decimal::from_str("0.0000001").unwrap());
        let total: Money = std::iter::repeat_n(one, 10_000).sum();
        assert_eq!(total, Money::new(Decimal::from_str("0.0010000").unwrap()));
    }
}
