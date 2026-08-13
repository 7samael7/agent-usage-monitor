//! Tasks side by side.
//!
//! Two things make a comparison honest, and both are done here rather than in
//! the interface.
//!
//! **Normalisation happens in decimal arithmetic.** "Cost per 1,000 output
//! tokens" is a division of money, and money crosses the wire as a string
//! precisely so nobody divides it as a float. Doing it in the browser would
//! undo that in one line.
//!
//! **The caveats are computed from the rows, not assumed.** Whether two tasks
//! are comparable depends on what was actually measured for each: if one
//! reports reasoning tokens and the other cannot, if one model has a price and
//! the other does not, if one task is still running — those are facts about
//! this comparison, and a table that does not carry them invites a conclusion
//! the data does not support.

use aum_contract::{
    Comparison, ComparisonRow, Currency, Measured, Money, Normalized, TaskMetrics, TaskStatus,
};
use aum_db::Database;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive as _;

use crate::prices::CostContext;

/// Output tokens per unit of normalisation.
const PER: u64 = 1_000;

/// Build a comparison of the given tasks, in the order asked for.
pub async fn compare(
    db: &Database,
    task_ids: &[uuid::Uuid],
    cost: CostContext<'_>,
    normalize: bool,
) -> Result<Comparison, aum_db::DbError> {
    let names = task_names(db).await?;
    let mut rows = Vec::with_capacity(task_ids.len());
    let mut missing = Vec::new();

    for &task_id in task_ids {
        // A task that is not in this database is not a task that did nothing.
        // Comparison selections travel in URLs, so they go stale — and a row of
        // zeros for a task that no longer exists is a confident claim about
        // something that was never measured.
        let Some((_, name)) = names.iter().find(|(id, _)| *id == task_id.to_string()) else {
            missing.push(task_id.to_string());
            continue;
        };

        let metrics = crate::task_metrics(db, task_id, cost).await?;
        let normalized = if normalize {
            normalize_row(&metrics)
        } else {
            None
        };
        rows.push(ComparisonRow {
            task_id,
            name: name.clone(),
            metrics: Box::new(metrics),
            normalized,
        });
    }

    let mut caveats = caveats(&rows, cost.currency);
    if !missing.is_empty() {
        caveats.insert(
            0,
            format!(
                "{} of the {} tasks asked for are not in this database and have been left out \
                 rather than shown as empty: {}.",
                missing.len(),
                task_ids.len(),
                missing.join(", ")
            ),
        );
    }

    Ok(Comparison { rows, caveats })
}

async fn task_names(db: &Database) -> Result<Vec<(String, String)>, aum_db::DbError> {
    sqlx::query_as("SELECT id, name FROM task")
        .fetch_all(db.reader())
        .await
        .map_err(Into::into)
}

/// Divide a task's figures by the output it produced.
///
/// `None` when there is no output yet: dividing by zero would produce either a
/// crash or an infinity, and both would be presented as a number.
fn normalize_row(m: &TaskMetrics) -> Option<Normalized> {
    let output = m.bands.output_total;
    if output == 0 {
        return None;
    }

    let scale = |count: u64| -> u64 {
        // Saturating rather than wrapping: a count large enough to overflow
        // here is a bug elsewhere, and a small wrong number would hide it.
        count.saturating_mul(PER) / output
    };

    let per_output = |measured: &Measured<u64>| -> Measured<u64> {
        // A ratio is never more certain than the count it came from, and a
        // division makes it calculated at best.
        measured.value.map_or_else(
            || Measured {
                value: None,
                accuracy: measured.accuracy.clone(),
            },
            |v| Measured {
                value: Some(scale(v)),
                accuracy: downgrade_to_calculated(&measured.accuracy),
            },
        )
    };

    let input_side = m.bands.input_fresh
        + m.bands.cache_read
        + m.bands.cache_write_5m
        + m.bands.cache_write_1h
        + m.bands.cache_write_unspecified;

    Some(Normalized {
        basis: format!("per {PER} output tokens"),
        denominator: output,
        total_tokens: per_output(&m.total_tokens),
        input_tokens: Measured {
            value: Some(scale(input_side)),
            accuracy: downgrade_to_calculated(&m.total_tokens.accuracy),
        },
        cost: normalize_money(&m.cost.api_equivalent, output),
    })
}

/// Money divided by output, in decimal.
fn normalize_money(amount: &Measured<Money>, output: u64) -> Measured<Money> {
    let Some(value) = amount.value.as_ref() else {
        // No cost to normalise. The original reason — an unpriced model, say —
        // is far more useful than anything this function could add.
        return Measured {
            value: None,
            accuracy: amount.accuracy.clone(),
        };
    };

    let Some(divisor) = Decimal::from_u64(output) else {
        return Measured {
            value: None,
            accuracy: amount.accuracy.clone(),
        };
    };

    let scaled = value
        .amount()
        .checked_mul(Decimal::from(PER))
        .and_then(|n| n.checked_div(divisor));

    scaled.map_or_else(
        || Measured {
            value: None,
            accuracy: amount.accuracy.clone(),
        },
        |v| Measured {
            value: Some(Money::new(v)),
            accuracy: downgrade_to_calculated(&amount.accuracy),
        },
    )
}

/// A derived figure can never be exact, whatever it was derived from.
///
/// Everything else keeps whatever it already was: a partial input divided by
/// something is still partial, and an unavailable one is still unavailable for
/// the reason it already had.
fn downgrade_to_calculated(accuracy: &aum_contract::Accuracy) -> aum_contract::Accuracy {
    use aum_contract::Accuracy;
    match accuracy {
        Accuracy::Exact { source } => Accuracy::Calculated { source: *source },
        other => other.clone(),
    }
}

/// What makes these rows less comparable than a table implies.
fn caveats(rows: &[ComparisonRow], currency: Currency) -> Vec<String> {
    let mut out = Vec::new();
    if rows.len() < 2 {
        return out;
    }

    let models: std::collections::BTreeSet<&str> = rows
        .iter()
        .filter_map(|r| r.metrics.model_id.as_deref())
        .collect();
    if models.len() > 1 {
        out.push(format!(
            "These tasks ran on different models ({}). Differences in tokens and cost are partly \
             differences between the models, not between the runs.",
            models.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    let with_reasoning = rows
        .iter()
        .filter(|r| r.metrics.reasoning_tokens.value.is_some())
        .count();
    if with_reasoning > 0 && with_reasoning < rows.len() {
        out.push(format!(
            "Reasoning tokens are reported for {with_reasoning} of {} tasks. The agents that do \
             not report them are not producing zero — the count is unavailable, so that column \
             cannot be totalled or ranked.",
            rows.len()
        ));
    }

    let unpriced = rows
        .iter()
        .filter(|r| r.metrics.cost.api_equivalent.value.is_none())
        .count();
    if unpriced > 0 {
        out.push(format!(
            "{unpriced} of {} tasks have no price for their model, so their cost is unavailable \
             rather than zero and the cost column cannot be ranked. A rate can be entered under \
             Models & Pricing.",
            rows.len()
        ));
    }

    let running = rows
        .iter()
        .filter(|r| r.metrics.status == TaskStatus::Running)
        .count();
    if running > 0 {
        out.push(format!(
            "{running} of {} tasks are still running, so their figures are a snapshot and will \
             grow.",
            rows.len()
        ));
    }

    let incomplete = rows
        .iter()
        .filter(|r| r.metrics.requests.is_lower_bound || r.metrics.requests.failed > 0)
        .count();
    if incomplete > 0 {
        out.push(format!(
            "{incomplete} of {} tasks have requests that could not be measured, so their totals \
             are lower bounds.",
            rows.len()
        ));
    }

    if currency != Currency::Usd {
        out.push(format!(
            "Costs are converted to {} from USD, which is what providers publish. A converted \
             amount is calculated rather than exact.",
            currency.code()
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use aum_contract::{
        Accuracy, CostBreakdown, DisplayKind, MeasurementSource, RequestCounts, TokenBands,
        UnavailableReason,
    };

    fn metrics(output: u64, model: &str, cost: Measured<Money>) -> TaskMetrics {
        TaskMetrics {
            task_id: uuid::Uuid::new_v4(),
            status: TaskStatus::Completed,
            model_id: Some(model.to_owned()),
            bands: TokenBands {
                input_fresh: 4_000,
                cache_read: 0,
                cache_write_5m: 0,
                cache_write_1h: 0,
                cache_write_unspecified: 0,
                output_total: output,
                reasoning: None,
                unclassified: 0,
            },
            total_tokens: Measured::exact(4_000 + output, MeasurementSource::ProviderReported),
            reasoning_tokens: Measured::unavailable(UnavailableReason::NotReportedByProvider {
                field: "reasoning".to_owned(),
                detail: "Claude Code does not report reasoning tokens.".to_owned(),
            }),
            requests: RequestCounts {
                succeeded: 1,
                failed: 0,
                retries: Some(0),
                is_lower_bound: false,
            },
            cost: CostBreakdown::subscription(Currency::Usd, cost, "team"),
            latency: aum_contract::LatencySummary::unavailable(
                "no latency in transcripts",
                "the local proxy",
            ),
            elapsed_ms: 10_000,
        }
    }

    fn row(m: TaskMetrics) -> ComparisonRow {
        let normalized = normalize_row(&m);
        ComparisonRow {
            task_id: m.task_id,
            name: "t".to_owned(),
            metrics: Box::new(m),
            normalized,
        }
    }

    fn usd(s: &str) -> Measured<Money> {
        Measured::calculated(
            Money::new(std::str::FromStr::from_str(s).unwrap()),
            MeasurementSource::ApplicationTelemetry,
        )
    }

    #[test]
    fn normalising_divides_by_output_and_says_what_it_divided_by() {
        let m = metrics(2_000, "claude-opus-5", usd("90.00"));
        let n = normalize_row(&m).unwrap();

        assert_eq!(n.denominator, 2_000);
        assert!(n.basis.contains("1000"), "{}", n.basis);
        // 6,000 total tokens over 2,000 output = 3,000 per 1,000 output.
        assert_eq!(n.total_tokens.value, Some(3_000));
        // $90 over 2,000 output = $45 per 1,000 output.
        assert_eq!(
            n.cost.value.unwrap().amount(),
            std::str::FromStr::from_str("45").unwrap()
        );
    }

    #[test]
    fn a_task_with_no_output_yet_normalises_to_nothing_rather_than_to_infinity() {
        assert!(normalize_row(&metrics(0, "m", usd("1.00"))).is_none());
    }

    #[test]
    fn a_normalised_figure_is_never_exact() {
        // The input here is Exact, straight from the provider. A ratio derived
        // from it is arithmetic we did, so it is calculated.
        let m = metrics(2_000, "m", usd("90.00"));
        assert_eq!(m.total_tokens.display_kind(), DisplayKind::Exact);
        let n = normalize_row(&m).unwrap();
        assert_eq!(n.total_tokens.display_kind(), DisplayKind::Calculated);
    }

    #[test]
    fn an_unpriced_task_normalises_to_unavailable_with_its_original_reason() {
        let missing = Measured::unavailable(UnavailableReason::NoPricingForModel {
            model_id: "gpt-5.6-sol".to_owned(),
        });
        let n = normalize_row(&metrics(2_000, "gpt-5.6-sol", missing)).unwrap();

        assert_eq!(n.cost.value, None);
        assert!(
            matches!(
                n.cost.accuracy,
                Accuracy::Unavailable {
                    reason: UnavailableReason::NoPricingForModel { .. }
                }
            ),
            "the reason must survive normalisation: {:?}",
            n.cost.accuracy
        );
    }

    #[test]
    fn comparing_different_models_says_so() {
        let rows = vec![
            row(metrics(1_000, "claude-opus-5", usd("1.00"))),
            row(metrics(1_000, "gpt-5.6-terra", usd("1.00"))),
        ];
        let text = caveats(&rows, Currency::Usd).join(" ");
        assert!(text.contains("different models"), "{text}");
        assert!(text.contains("claude-opus-5"), "{text}");
    }

    #[test]
    fn an_unpriced_row_makes_the_cost_column_unrankable_and_says_where_to_fix_it() {
        let rows = vec![
            row(metrics(1_000, "m", usd("1.00"))),
            row(metrics(
                1_000,
                "m",
                Measured::unavailable(UnavailableReason::NoPricingForModel {
                    model_id: "m".to_owned(),
                }),
            )),
        ];
        let text = caveats(&rows, Currency::Usd).join(" ");
        assert!(text.contains("cannot be ranked"), "{text}");
        assert!(text.contains("Models & Pricing"), "{text}");
    }

    #[test]
    fn a_single_row_needs_no_caveats() {
        // Nothing is being compared, so there is nothing to warn about.
        let rows = vec![row(metrics(1_000, "m", usd("1.00")))];
        assert!(caveats(&rows, Currency::Usd).is_empty());
    }

    #[test]
    fn a_converted_comparison_says_the_amounts_were_converted() {
        let rows = vec![
            row(metrics(1_000, "m", usd("1.00"))),
            row(metrics(1_000, "m", usd("2.00"))),
        ];
        let text = caveats(&rows, Currency::Eur).join(" ");
        assert!(text.contains("converted to EUR"), "{text}");
    }
}
