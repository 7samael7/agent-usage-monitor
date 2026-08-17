//! # `aum-pricing` — what usage would have cost
//!
//! Three rules shape everything here.
//!
//! **No floats.** `rust_decimal` end to end. A cent cannot be represented in
//! binary floating point, and the amounts involved are frequently in the 1e-7
//! range and summed over tens of thousands of requests.
//!
//! **Round once, at the end.** Each request's cost is computed exactly and the
//! exact values are summed; the result is rounded only for display. Rounding
//! per request and then summing drifts, and forfeits any claim that the total
//! is exact.
//!
//! **Never substitute a price.** A model with no entry yields
//! [`CostOutcome::Unavailable`], not a "similar" model's rate and not zero.
//! This is not hypothetical: `claude-opus-5`, `claude-fable-5`, `gpt-5.6-sol`
//! and `gpt-5.6-terra` all appear in this machine's real data and in no public
//! price list. Defaulting them to zero would show the heaviest sessions on the
//! machine as free.

pub mod fx;
pub mod nano;
pub mod table;

use aum_contract::{Measured, MeasurementSource, Money, UnavailableReason};
use aum_domain::TokenUsage;
use rust_decimal::Decimal;

pub use fx::{ExchangeRate, FxError, convert};
pub use table::{ModelPricing, PriceTable, Rates};

/// Tokens are priced per million.
fn per_million(tokens: u64, rate_per_mtok: Decimal) -> Decimal {
    Decimal::from(tokens)
        .checked_mul(rate_per_mtok)
        .and_then(|v| v.checked_div(Decimal::from(1_000_000_u32)))
        .unwrap_or(Decimal::ZERO)
}

/// The result of trying to price one measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CostOutcome {
    /// A cost, and the price version it was computed from.
    Priced {
        amount: Decimal,
        pricing_version: String,
        /// Tokens that could not be priced because the provider did not say
        /// what they were. The amount is a floor when this is non-zero.
        unpriceable_tokens: u64,
    },
    Unavailable(UnavailableReason),
}

/// Cost one request's worth of usage.
///
/// The signature takes a [`TokenUsage`] and nothing else that could carry a
/// provider's raw fields: the disjoint partition is the only way in, so the
/// double-counting mistakes that would otherwise be available here — charging
/// cached input twice, billing reasoning on top of output — are not expressible.
#[must_use]
pub fn cost_of(usage: &TokenUsage, model_id: Option<&str>, table: &PriceTable) -> CostOutcome {
    let Some(model_id) = model_id else {
        // A tailer that resumed mid-file genuinely does not know the model.
        return CostOutcome::Unavailable(UnavailableReason::ModelUnknown {
            detail: "No model has been observed for this request, so it cannot be priced."
                .to_owned(),
        });
    };

    // Locally generated messages are not API calls.
    if model_id == "<synthetic>" {
        return CostOutcome::Priced {
            amount: Decimal::ZERO,
            pricing_version: "local".to_owned(),
            unpriceable_tokens: 0,
        };
    }

    let Some(pricing) = table.lookup(model_id) else {
        return CostOutcome::Unavailable(UnavailableReason::NoPricingForModel {
            model_id: model_id.to_owned(),
        });
    };
    let r = &pricing.rates;

    let mut amount = per_million(usage.input_fresh(), r.input_per_mtok);
    amount += per_million(usage.output_total(), r.output_per_mtok);
    amount += per_million(usage.cache_read(), r.cache_read_per_mtok);

    // The TTL split is load-bearing: a 1-hour write costs roughly 2x input
    // where a 5-minute write costs 1.25x, and real Claude sessions are
    // dominated by 1-hour writes. Collapsing them understates by about a
    // quarter.
    amount += per_million(usage.cache_write_5m(), r.cache_write_5m_per_mtok);
    amount += per_million(usage.cache_write_1h(), r.cache_write_1h_per_mtok);

    // A cache write whose TTL the provider did not break down. Priced at the
    // cheaper tier deliberately, so the figure is a floor rather than a guess
    // that could overstate.
    amount += per_million(usage.cache_write_unspecified(), r.cache_write_5m_per_mtok);

    // Reasoning is a subset of output and is already counted above. Adding it
    // again would overstate by 4% and rising on reasoning-heavy models.

    CostOutcome::Priced {
        amount,
        pricing_version: pricing.version_id.clone(),
        // Tokens the provider counted but did not classify cannot be priced:
        // input and output rates differ by roughly eight times, so attributing
        // them to either side would be a material error rather than a rounding
        // one.
        unpriceable_tokens: usage.unclassified(),
    }
}

/// Cost a whole task, exactly, rounding only at the end.
///
/// Returns a [`Measured`] so the caller cannot lose the distinction between
/// "this cost nothing" and "this could not be priced".
#[must_use]
pub fn cost_of_many(items: &[(TokenUsage, Option<String>)], table: &PriceTable) -> Measured<Money> {
    if items.is_empty() {
        return Measured::unavailable(UnavailableReason::NoTelemetry {
            detail: "Nothing has been measured yet.".to_owned(),
        });
    }

    let mut total = Decimal::ZERO;
    let mut priced = 0_u32;
    let mut unpriceable_tokens = 0_u64;
    let mut first_missing: Option<UnavailableReason> = None;

    for (usage, model) in items {
        match cost_of(usage, model.as_deref(), table) {
            CostOutcome::Priced {
                amount,
                unpriceable_tokens: skipped,
                ..
            } => {
                total += amount;
                priced = priced.saturating_add(1);
                unpriceable_tokens = unpriceable_tokens.saturating_add(skipped);
            }
            CostOutcome::Unavailable(reason) => {
                if first_missing.is_none() {
                    first_missing = Some(reason);
                }
            }
        }
    }

    let count = u32::try_from(items.len()).unwrap_or(u32::MAX);

    if priced == 0 {
        return Measured::unavailable(first_missing.unwrap_or(UnavailableReason::NoTelemetry {
            detail: "None of these requests could be priced.".to_owned(),
        }));
    }

    // Our own calculation from our own table, so `Calculated` rather than
    // `Exact` — the provider did not tell us this number.
    if priced == count && unpriceable_tokens == 0 {
        return Measured::calculated(Money::new(total), MeasurementSource::ApplicationTelemetry);
    }

    let mut why = Vec::new();
    if priced < count {
        why.push(format!(
            "{} of {count} requests have no price for their model",
            count.saturating_sub(priced)
        ));
    }
    if unpriceable_tokens > 0 {
        why.push(format!(
            "{unpriceable_tokens} tokens were counted by the provider without an input/output \
             split and cannot be priced"
        ));
    }

    Measured::partial(Money::new(total), priced, count, why.join("; "))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use aum_contract::DisplayKind;
    use aum_domain::{AnthropicCacheCreation, AnthropicUsage, OpenAiUsage};
    use std::str::FromStr as _;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    /// A table with the illustrative rates used throughout the docs.
    fn table() -> PriceTable {
        PriceTable::from_entries(vec![
            ModelPricing {
                version_id: "opus-test".to_owned(),
                model_id: "claude-opus-5".to_owned(),
                rates: Rates {
                    input_per_mtok: dec("15.00"),
                    output_per_mtok: dec("75.00"),
                    cache_read_per_mtok: dec("1.50"),
                    cache_write_5m_per_mtok: dec("18.75"),
                    cache_write_1h_per_mtok: dec("30.00"),
                },
                effective_from: "2026-01-01T00:00:00.000Z".to_owned(),
                source: "seed".to_owned(),
                note: None,
            },
            ModelPricing {
                version_id: "gpt-test".to_owned(),
                model_id: "gpt-5.5".to_owned(),
                rates: Rates {
                    input_per_mtok: dec("1.25"),
                    output_per_mtok: dec("10.00"),
                    cache_read_per_mtok: dec("0.125"),
                    cache_write_5m_per_mtok: dec("1.25"),
                    cache_write_1h_per_mtok: dec("1.25"),
                },
                effective_from: "2026-01-01T00:00:00.000Z".to_owned(),
                source: "seed".to_owned(),
                note: None,
            },
        ])
    }

    /// The real Claude request used throughout: input 2, 1-hour cache write
    /// 23,610, cache read 30,885, output 4,452.
    fn real_claude() -> TokenUsage {
        TokenUsage::from_anthropic(&AnthropicUsage {
            input_tokens: Some(2),
            output_tokens: Some(4_452),
            cache_creation_input_tokens: Some(23_610),
            cache_read_input_tokens: Some(30_885),
            cache_creation: Some(AnthropicCacheCreation {
                ephemeral_5m_input_tokens: Some(0),
                ephemeral_1h_input_tokens: Some(23_610),
            }),
            ..Default::default()
        })
        .unwrap()
        .usage
    }

    /// The real Codex turn: input 16,210 of which 11,008 cached, output 164
    /// including 35 reasoning.
    fn real_codex() -> TokenUsage {
        TokenUsage::from_openai(&OpenAiUsage {
            input_tokens: Some(16_210),
            cached_input_tokens: Some(11_008),
            output_tokens: Some(164),
            reasoning_output_tokens: Some(35),
            total_tokens: Some(16_374),
            ..Default::default()
        })
        .unwrap()
        .usage
    }

    #[test]
    fn a_real_claude_request_costs_what_the_worked_example_says() {
        //   input_fresh          2 @ 15.00 = 0.0000300
        //   cache_write_1h  23,610 @ 30.00 = 0.7083000
        //   cache_read      30,885 @  1.50 = 0.0463275
        //   output           4,452 @ 75.00 = 0.3339000
        //                                  ------------
        //                                    1.0885575
        //
        // Verified independently. The worked example in the original design
        // note read 1.0885275, which dropped its own input line.
        let CostOutcome::Priced { amount, .. } =
            cost_of(&real_claude(), Some("claude-opus-5"), &table())
        else {
            panic!("expected a price")
        };
        assert_eq!(amount, dec("1.0885575"));
    }

    #[test]
    fn pricing_cache_writes_at_a_flat_rate_would_understate_by_a_quarter() {
        // The trap: treating every cache write as the 5-minute tier. Real
        // Claude sessions are dominated by 1-hour writes.
        let usage = real_claude();
        let t = table();
        let CostOutcome::Priced {
            amount: correct, ..
        } = cost_of(&usage, Some("claude-opus-5"), &t)
        else {
            panic!()
        };

        let flat = per_million(usage.input_fresh(), dec("15.00"))
            + per_million(usage.cache_write_1h(), dec("18.75"))
            + per_million(usage.cache_read(), dec("1.50"))
            + per_million(usage.output_total(), dec("75.00"));

        assert_eq!(flat, dec("0.8229450"));
        let understatement = (correct - flat) / correct;
        assert!(
            understatement > dec("0.20") && understatement < dec("0.28"),
            "expected roughly a quarter, got {understatement}"
        );
    }

    #[test]
    fn a_real_codex_turn_costs_what_the_worked_example_says() {
        //   input_fresh  5,202 @ 1.25  = 0.00650250
        //   cache_read  11,008 @ 0.125 = 0.00137600
        //   output         164 @ 10.00 = 0.00164000
        let CostOutcome::Priced { amount, .. } = cost_of(&real_codex(), Some("gpt-5.5"), &table())
        else {
            panic!("expected a price")
        };
        assert_eq!(amount, dec("0.0095185"));
    }

    #[test]
    fn reasoning_is_not_billed_on_top_of_output() {
        // The 4% trap. Reasoning is a subset of output and is already counted.
        let usage = real_codex();
        let CostOutcome::Priced { amount, .. } = cost_of(&usage, Some("gpt-5.5"), &table()) else {
            panic!()
        };
        let double_counted = amount + per_million(usage.reasoning().unwrap(), dec("10.00"));
        assert!(double_counted > amount);
        assert_eq!(
            amount,
            dec("0.0095185"),
            "reasoning must not be added again"
        );
    }

    #[test]
    fn cached_input_is_not_charged_at_the_full_rate() {
        // The 145% trap, prevented upstream by the disjoint partition: there is
        // no way to reach this function with cached tokens still inside input.
        let usage = real_codex();
        assert_eq!(usage.input_fresh(), 5_202);
        assert_eq!(usage.cache_read(), 11_008);

        let CostOutcome::Priced { amount, .. } = cost_of(&usage, Some("gpt-5.5"), &table()) else {
            panic!()
        };
        let naive = per_million(16_210, dec("1.25"))
            + per_million(11_008, dec("0.125"))
            + per_million(164, dec("10.00"));
        assert!(
            naive > amount * dec("2"),
            "the naive figure is 2.4x the truth"
        );
    }

    #[test]
    fn an_unknown_model_is_unavailable_rather_than_free() {
        // Live on this machine: gpt-5.6-sol and claude-opus-5 appear in real
        // data and in no public price list. Defaulting to zero would show the
        // heaviest sessions here as costing nothing.
        let outcome = cost_of(&real_codex(), Some("gpt-5.6-sol"), &table());
        assert!(matches!(
            outcome,
            CostOutcome::Unavailable(UnavailableReason::NoPricingForModel { .. })
        ));
    }

    #[test]
    fn a_request_with_no_known_model_is_unavailable() {
        assert!(matches!(
            cost_of(&real_codex(), None, &table()),
            CostOutcome::Unavailable(UnavailableReason::ModelUnknown { .. })
        ));
    }

    #[test]
    fn a_locally_generated_message_costs_nothing_and_says_so() {
        let outcome = cost_of(&TokenUsage::default(), Some("<synthetic>"), &table());
        assert!(matches!(
            outcome,
            CostOutcome::Priced { amount, .. } if amount == Decimal::ZERO
        ));
    }

    #[test]
    fn unclassified_tokens_are_reported_as_unpriceable() {
        // Codex's compaction calls: a total with no input/output split.
        let usage = TokenUsage::unclassified_only(15_683);
        let CostOutcome::Priced {
            amount,
            unpriceable_tokens,
            ..
        } = cost_of(&usage, Some("gpt-5.5"), &table())
        else {
            panic!()
        };
        assert_eq!(amount, Decimal::ZERO);
        assert_eq!(unpriceable_tokens, 15_683);
    }

    #[test]
    fn a_task_of_priceable_requests_is_calculated_not_exact() {
        // We computed it from our own table; the provider never said it.
        let items = vec![
            (real_claude(), Some("claude-opus-5".to_owned())),
            (real_codex(), Some("gpt-5.5".to_owned())),
        ];
        let m = cost_of_many(&items, &table());
        assert_eq!(m.display_kind(), DisplayKind::Calculated);
        assert_eq!(
            m.value.unwrap().amount(),
            dec("1.0885575") + dec("0.0095185")
        );
    }

    #[test]
    fn one_unpriced_model_makes_the_task_total_a_floor() {
        let items = vec![
            (real_claude(), Some("claude-opus-5".to_owned())),
            (real_codex(), Some("gpt-5.6-sol".to_owned())),
        ];
        let m = cost_of_many(&items, &table());
        assert_eq!(m.display_kind(), DisplayKind::Partial);
        assert_eq!(m.value.unwrap().amount(), dec("1.0885575"));
    }

    #[test]
    fn a_task_where_nothing_can_be_priced_is_unavailable_not_zero() {
        let items = vec![(real_codex(), Some("gpt-5.6-sol".to_owned()))];
        let m = cost_of_many(&items, &table());
        assert_eq!(m.value, None);
        assert_eq!(m.display_kind(), DisplayKind::Unavailable);
    }

    #[test]
    fn summing_exactly_beats_rounding_each_request() {
        // Ten thousand small requests. Rounding each to four places and then
        // summing loses money that summing exactly keeps.
        let tiny = TokenUsage::from_openai(&OpenAiUsage {
            input_tokens: Some(1),
            output_tokens: Some(1),
            total_tokens: Some(2),
            ..Default::default()
        })
        .unwrap()
        .usage;

        let items: Vec<_> = (0..10_000)
            .map(|_| (tiny, Some("gpt-5.5".to_owned())))
            .collect();
        let exact = cost_of_many(&items, &table()).value.unwrap().amount();

        let per_request = match cost_of(&tiny, Some("gpt-5.5"), &table()) {
            CostOutcome::Priced { amount, .. } => amount,
            CostOutcome::Unavailable(_) => panic!(),
        };
        let rounded_then_summed = per_request.round_dp(4) * Decimal::from(10_000_u32);

        assert_ne!(
            exact, rounded_then_summed,
            "if these ever match, the test has stopped proving anything"
        );
        assert_eq!(
            rounded_then_summed,
            Decimal::ZERO,
            "each request rounds to nothing"
        );
        assert!(
            exact > Decimal::ZERO,
            "but ten thousand of them are not nothing"
        );
    }

    #[test]
    fn an_empty_task_is_unavailable_rather_than_zero() {
        assert_eq!(cost_of_many(&[], &table()).value, None);
    }
}
