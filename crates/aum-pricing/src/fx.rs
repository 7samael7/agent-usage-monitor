//! Currency conversion.
//!
//! USD is canonical, because that is what providers publish. EUR and CZK are
//! presentation, produced by applying a dated rate.
//!
//! A converted amount is **at best calculated**, never exact — it depends on a
//! rate that was true at some moment and is not now. An amount converted with a
//! stale rate says so, with the date, rather than being quietly presented as
//! current. Offline is a normal state, not an error; pretending an old rate is
//! today's would be a small lie compounding on top of an already-approximate
//! cost.

use aum_contract::{Currency, Measured, MeasurementSource, Money, UnavailableReason};
use rust_decimal::Decimal;

/// A rate, and when it was true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangeRate {
    pub quote: Currency,
    /// Units of `quote` per 1 USD.
    pub rate: Decimal,
    pub as_of: chrono::DateTime<chrono::Utc>,
    pub source: String,
}

/// A rate older than this is shown as stale, and any amount converted with it
/// is downgraded. Providers change prices rarely; currencies move daily.
pub const STALE_AFTER_DAYS: i64 = 7;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FxError {
    #[error("no exchange rate is available for {0:?}")]
    NoRate(Currency),
    #[error("the exchange rate for {currency:?} is not a usable number")]
    Unusable { currency: Currency },
}

impl ExchangeRate {
    #[must_use]
    pub fn age_days(&self, now: chrono::DateTime<chrono::Utc>) -> i64 {
        now.signed_duration_since(self.as_of).num_days()
    }

    #[must_use]
    pub fn is_stale(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.age_days(now) >= STALE_AFTER_DAYS
    }
}

/// Convert an amount from USD.
///
/// Returns a [`Measured`] rather than a bare amount, so the loss of certainty
/// cannot be dropped on the way to the screen.
#[must_use]
pub fn convert(
    amount: Measured<Money>,
    to: Currency,
    rate: Option<&ExchangeRate>,
    now: chrono::DateTime<chrono::Utc>,
) -> Measured<Money> {
    // USD needs no rate, so it also loses no certainty.
    if to == Currency::Usd {
        return amount;
    }

    let Some(value) = amount.value else {
        // Nothing to convert. The original reason is more useful than an FX one.
        return amount;
    };

    let Some(rate) = rate else {
        return Measured::unavailable(UnavailableReason::NoTelemetry {
            detail: format!(
                "No exchange rate is available for {}, so this amount cannot be shown in that \
                 currency. It is available in USD.",
                to.code()
            ),
        });
    };

    let converted = value
        .amount()
        .checked_mul(rate.rate)
        .map(Money::new)
        .unwrap_or(Money::ZERO);

    // A converted figure was never exact, whatever the source amount was: it
    // depends on a rate that was true at a moment, not now.
    if rate.is_stale(now) {
        return Measured::estimated(converted);
    }
    Measured::calculated(converted, MeasurementSource::ApplicationTelemetry)
}

/// How a rate should be described in the UI.
#[must_use]
pub fn describe(rate: Option<&ExchangeRate>, now: chrono::DateTime<chrono::Utc>) -> String {
    match rate {
        None => "No exchange rate available.".to_owned(),
        Some(r) if r.is_stale(now) => format!(
            "FX rate {} days old, from {} ({}). Amounts shown in {} are approximate.",
            r.age_days(now),
            r.as_of.format("%Y-%m-%d"),
            r.source,
            r.quote.code()
        ),
        Some(r) => format!(
            "FX rate updated {} ({}).",
            r.as_of.format("%Y-%m-%d"),
            r.source
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use aum_contract::DisplayKind;
    use std::str::FromStr as _;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-08-13T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn rate(days_old: i64) -> ExchangeRate {
        ExchangeRate {
            quote: Currency::Eur,
            rate: Decimal::from_str("0.92").unwrap(),
            as_of: now() - chrono::Duration::days(days_old),
            source: "manual".to_owned(),
        }
    }

    fn usd(amount: &str) -> Measured<Money> {
        Measured::calculated(
            Money::new(Decimal::from_str(amount).unwrap()),
            MeasurementSource::ApplicationTelemetry,
        )
    }

    #[test]
    fn usd_needs_no_conversion_and_loses_no_certainty() {
        let original = usd("1.0885275");
        let converted = convert(original.clone(), Currency::Usd, None, now());
        assert_eq!(converted, original);
    }

    #[test]
    fn a_fresh_rate_converts_and_stays_calculated() {
        let m = convert(usd("100"), Currency::Eur, Some(&rate(1)), now());
        assert_eq!(
            m.value.unwrap().amount(),
            Decimal::from_str("92.00").unwrap()
        );
        assert_eq!(m.display_kind(), DisplayKind::Calculated);
    }

    #[test]
    fn a_stale_rate_downgrades_the_amount_to_estimated() {
        // Offline is normal. Presenting a twelve-day-old rate as current would
        // be a small lie on top of an already-approximate cost.
        let m = convert(usd("100"), Currency::Eur, Some(&rate(12)), now());
        assert_eq!(
            m.value.unwrap().amount(),
            Decimal::from_str("92.00").unwrap()
        );
        assert_eq!(m.display_kind(), DisplayKind::Estimated);
    }

    #[test]
    fn a_missing_rate_yields_unavailable_rather_than_the_usd_figure() {
        // Showing the dollar amount under a CZK label would be worse than
        // showing nothing.
        let m = convert(usd("100"), Currency::Czk, None, now());
        assert_eq!(m.value, None);
        assert_eq!(m.display_kind(), DisplayKind::Unavailable);
    }

    #[test]
    fn an_unavailable_amount_keeps_its_original_reason() {
        // "No price for this model" is more useful than "no exchange rate".
        let missing: Measured<Money> =
            Measured::unavailable(UnavailableReason::NoPricingForModel {
                model_id: "gpt-5.6-sol".to_owned(),
            });
        let m = convert(missing, Currency::Eur, Some(&rate(1)), now());
        assert!(matches!(
            m.accuracy,
            aum_contract::Accuracy::Unavailable {
                reason: UnavailableReason::NoPricingForModel { .. }
            }
        ));
    }

    #[test]
    fn staleness_is_dated_rather_than_merely_flagged() {
        let text = describe(Some(&rate(12)), now());
        assert!(text.contains("12 days old"), "{text}");
        assert!(
            text.contains("2026-08-01"),
            "the date must be shown: {text}"
        );
    }

    #[test]
    fn a_fresh_rate_is_described_with_its_date() {
        let text = describe(Some(&rate(1)), now());
        assert!(text.contains("2026-08-12"), "{text}");
        assert!(!text.contains("approximate"));
    }

    #[test]
    fn no_rate_at_all_is_stated_plainly() {
        assert!(describe(None, now()).contains("No exchange rate"));
    }

    #[test]
    fn the_staleness_boundary_is_where_it_says_it_is() {
        assert!(!rate(STALE_AFTER_DAYS - 1).is_stale(now()));
        assert!(rate(STALE_AFTER_DAYS).is_stale(now()));
    }
}
