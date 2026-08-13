//! Turning stored rows into numbers the UI may display.
//!
//! This is where the honesty rules stop being a policy and become code. Three
//! decisions are made here and nowhere else:
//!
//! 1. **When a total may call itself exact.** Only when every contributing
//!    request was measured and nothing about the observation window is
//!    incomplete. Twelve exact requests and one unmeasured one is `Partial`,
//!    rendered with `≥`.
//! 2. **What "unavailable" means for a metric no capture level can produce.**
//!    Latency from transcripts is the case: the reason names the remedy rather
//!    than leaving a bare dash.
//! 3. **How a metric that only some providers report is aggregated.** Reasoning
//!    tokens are reported by Codex and not by Claude Code, so a mixed task
//!    reports a floor over the requests that have it — never a sum that treats
//!    the others as zero.

use aum_contract::{
    CostBreakdown, Currency, LatencySummary, Measured, MeasurementSource, Money, RequestCounts,
    TaskMetrics, TaskStatus, TokenBands, UnavailableReason,
};
use aum_db::repo::TaskTotals;

/// What is known about how complete an observation is.
///
/// Kept separate from per-request accuracy because they fail independently:
/// thirteen perfectly exact requests observed from halfway through a session is
/// still not an exact total, and a source-only check would miss that entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Completeness {
    /// Requests we know happened but could not measure.
    pub unmeasured_requests: u32,
    /// Monitoring began after the session had already started.
    pub started_mid_session: bool,
    /// An ingest anomaly was recorded against this task's sessions.
    pub anomalies: u32,
}

impl Completeness {
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.unmeasured_requests == 0 && !self.started_mid_session && self.anomalies == 0
    }

    /// What is missing, in a sentence.
    ///
    /// Carried on the measurement itself, because the counts alone can mislead:
    /// a task observed from halfway through has measured every request it saw,
    /// so `13 of 13` would read as complete when it is not.
    #[must_use]
    pub fn reason(&self) -> String {
        let mut parts = Vec::new();
        if self.unmeasured_requests > 0 {
            parts.push(format!(
                "{} request(s) could not be measured",
                self.unmeasured_requests
            ));
        }
        if self.started_mid_session {
            parts.push("monitoring began after this session had started".to_owned());
        }
        if self.anomalies > 0 {
            parts.push(format!(
                "{} ingest anomaly/anomalies recorded",
                self.anomalies
            ));
        }
        parts.join("; ")
    }
}

/// Everything needed to describe a task's consumption.
pub struct MetricsInput<'a> {
    pub task_id: uuid::Uuid,
    pub status: TaskStatus,
    pub totals: &'a TaskTotals,
    pub completeness: &'a Completeness,
    pub model_id: Option<String>,
    pub elapsed_ms: u64,
    pub failed_requests: u32,
    /// `None` where the agent does not expose retries at all.
    pub retries: Option<u32>,
    /// Which agent, so the latency explanation can name the right remedy.
    pub adapter_id: &'a str,
    pub currency: Currency,
    /// Our own cost figure, if the pricing engine could produce one.
    pub api_equivalent: Measured<Money>,
    /// The subscription plan, when usage is not billed per token.
    pub subscription_plan: Option<String>,
}

/// Build the snapshot the dashboard renders.
#[must_use]
pub fn build(input: &MetricsInput<'_>) -> TaskMetrics {
    let t = input.totals;
    let bands = TokenBands {
        input_fresh: as_u64(t.input_fresh),
        cache_read: as_u64(t.cache_read),
        cache_write_5m: as_u64(t.cache_write_5m),
        cache_write_1h: as_u64(t.cache_write_1h),
        cache_write_unspecified: as_u64(t.cache_write_unspecified),
        output_total: as_u64(t.output_total),
        reasoning: t.reasoning.map(as_u64),
        unclassified: as_u64(t.unclassified),
    };

    let measured_requests = u32::try_from(t.requests).unwrap_or(u32::MAX);

    TaskMetrics {
        task_id: input.task_id,
        status: input.status,
        bands,
        total_tokens: total_tokens(&bands, measured_requests, input.completeness),
        reasoning_tokens: reasoning_tokens(t, measured_requests),
        requests: RequestCounts {
            succeeded: measured_requests,
            failed: input.failed_requests,
            retries: input.retries,
            is_lower_bound: !input.completeness.is_complete(),
        },
        elapsed_ms: input.elapsed_ms,
        model_id: input.model_id.clone(),
        cost: cost(input),
        latency: latency(input.adapter_id),
    }
}

/// The headline number.
///
/// Reachable as `Exact` only through the one path where nothing is missing.
fn total_tokens(bands: &TokenBands, measured: u32, completeness: &Completeness) -> Measured<u64> {
    let total = bands.grand_total();

    if measured == 0 {
        return Measured::unavailable(UnavailableReason::NoTelemetry {
            detail: "No requests have been measured for this task yet.".to_owned(),
        });
    }

    if completeness.is_complete() {
        return Measured::exact(total, MeasurementSource::ProviderReported);
    }

    let observed = measured.saturating_add(completeness.unmeasured_requests);
    Measured::partial(total, measured, observed, completeness.reason())
}

/// Reasoning tokens, where only some providers report them.
///
/// A mixed task must not sum the reporting requests and present the result as
/// if it covered all of them.
fn reasoning_tokens(t: &TaskTotals, measured: u32) -> Measured<u64> {
    let reported_by = u32::try_from(t.reasoning_reported_by).unwrap_or(u32::MAX);

    match t.reasoning {
        // No contributing request reported reasoning at all. This is the normal
        // state for a Claude Code task, and it is not zero.
        None => Measured::unavailable(UnavailableReason::NotReportedByProvider {
            field: "reasoning_tokens".to_owned(),
            detail: "This agent does not report a reasoning-token count.".to_owned(),
        }),
        Some(value) if reported_by >= measured && measured > 0 => {
            Measured::exact(as_u64(value), MeasurementSource::ProviderReported)
        }
        Some(value) => Measured::partial(
            as_u64(value),
            reported_by,
            measured,
            format!(
                "{reported_by} of {measured} requests report reasoning tokens; the rest come \
                 from an agent that does not expose them, so this is a lower bound"
            ),
        ),
    }
}

fn cost(input: &MetricsInput<'_>) -> CostBreakdown {
    let actual_billed = match &input.subscription_plan {
        // Genuinely unknowable: a subscription does not bill per token, so
        // there is no per-task charge to report. Saying so is the point.
        Some(plan) => {
            Measured::unavailable(UnavailableReason::SubscriptionBilled { plan: plan.clone() })
        }
        None => Measured::unavailable(UnavailableReason::NoTelemetry {
            detail: "No billing information is available for this agent.".to_owned(),
        }),
    };

    CostBreakdown {
        currency: input.currency,
        api_equivalent: input.api_equivalent.clone(),
        // Neither adapter's files carry a cost figure. Claude Code can report
        // one through OpenTelemetry, which is a later capture level.
        provider_reported: Measured::unavailable(UnavailableReason::RequiresCaptureLevel {
            level: "OpenTelemetry export".to_owned(),
            detail: "This agent does not write a cost figure to its transcripts.".to_owned(),
        }),
        actual_billed,
    }
}

/// Latency, which transcripts simply do not contain.
///
/// The tempting alternative is the gap between message timestamps. That number
/// is plausible, well-scaled and wrong: the interval contains tool execution,
/// retry backoff, queueing and user think time, and because one response is
/// written across several lines seconds apart, even its endpoints are ambiguous.
/// It is not offered here in any form.
fn latency(adapter_id: &str) -> LatencySummary {
    let detail = match adapter_id {
        "claude_code" => "Claude Code transcripts contain no latency or time-to-first-token field.",
        "codex" => "Codex rollouts contain no per-request latency field.",
        _ => "This capture method does not observe request timing.",
    };
    LatencySummary::unavailable(detail, "the local proxy or OpenTelemetry export")
}

fn as_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use aum_contract::DisplayKind;

    fn totals() -> TaskTotals {
        TaskTotals {
            requests: 13,
            input_fresh: 100,
            cache_read: 2_000,
            cache_write_1h: 500,
            output_total: 400,
            reasoning: None,
            reasoning_reported_by: 0,
            ..Default::default()
        }
    }

    fn input<'a>(
        totals: &'a TaskTotals,
        completeness: &'a Completeness,
        adapter: &'a str,
    ) -> MetricsInput<'a> {
        MetricsInput {
            task_id: uuid::Uuid::nil(),
            status: TaskStatus::Running,
            totals,
            completeness,
            model_id: Some("claude-opus-5".to_owned()),
            elapsed_ms: 60_000,
            failed_requests: 0,
            retries: Some(0),
            adapter_id: adapter,
            currency: Currency::Usd,
            api_equivalent: Measured::unavailable(UnavailableReason::NoPricingForModel {
                model_id: "claude-opus-5".to_owned(),
            }),
            subscription_plan: Some("team".to_owned()),
        }
    }

    #[test]
    fn a_complete_task_of_measured_requests_is_exact() {
        let t = totals();
        let c = Completeness::default();
        let m = build(&input(&t, &c, "claude_code"));
        assert_eq!(m.total_tokens.display_kind(), DisplayKind::Exact);
        assert_eq!(m.total_tokens.value, Some(3_000));
    }

    #[test]
    fn one_unmeasured_request_downgrades_the_total_to_partial() {
        // Twelve exact requests plus one unmeasured must never render as EXACT.
        let t = totals();
        let c = Completeness {
            unmeasured_requests: 1,
            ..Default::default()
        };
        let m = build(&input(&t, &c, "claude_code"));

        assert_eq!(m.total_tokens.display_kind(), DisplayKind::Partial);
        assert!(matches!(
            m.total_tokens.accuracy,
            aum_contract::Accuracy::Partial {
                measured: 13,
                total: 14,
                ..
            }
        ));
        assert!(m.requests.is_lower_bound, "counts are a floor");
    }

    #[test]
    fn starting_mid_session_is_enough_to_stop_a_total_being_exact() {
        // The failure a source-only check misses: every request perfectly
        // measured, over a window that began late.
        let t = totals();
        let c = Completeness {
            started_mid_session: true,
            ..Default::default()
        };
        let m = build(&input(&t, &c, "claude_code"));
        assert_ne!(m.total_tokens.display_kind(), DisplayKind::Exact);

        // ...and it must say so. The counts alone read as complete here — every
        // request seen was measured — so without the reason the UI would show
        // "13 of 13" beside a partial badge and look like a bug rather than a
        // caveat.
        let aum_contract::Accuracy::Partial { reason, .. } = &m.total_tokens.accuracy else {
            panic!("expected partial, got {:?}", m.total_tokens.accuracy)
        };
        assert!(
            reason.contains("began after"),
            "the reason must explain the gap: {reason}"
        );
    }

    #[test]
    fn an_ingest_anomaly_stops_a_total_being_exact() {
        let t = totals();
        let c = Completeness {
            anomalies: 1,
            ..Default::default()
        };
        assert_ne!(
            build(&input(&t, &c, "claude_code"))
                .total_tokens
                .display_kind(),
            DisplayKind::Exact
        );
    }

    #[test]
    fn a_task_with_nothing_measured_reports_unavailable_not_zero() {
        let t = TaskTotals::default();
        let c = Completeness::default();
        let m = build(&input(&t, &c, "claude_code"));
        assert_eq!(m.total_tokens.value, None);
        assert_eq!(m.total_tokens.display_kind(), DisplayKind::Unavailable);
    }

    #[test]
    fn claude_code_reports_reasoning_as_unavailable_rather_than_zero() {
        let t = totals();
        let c = Completeness::default();
        let m = build(&input(&t, &c, "claude_code"));

        assert_eq!(m.reasoning_tokens.value, None);
        assert_eq!(m.reasoning_tokens.display_kind(), DisplayKind::Unavailable);
        assert_ne!(m.reasoning_tokens.value, Some(0));
    }

    #[test]
    fn codex_reports_reasoning_exactly_when_every_request_has_it() {
        let t = TaskTotals {
            requests: 5,
            reasoning: Some(175),
            reasoning_reported_by: 5,
            output_total: 800,
            ..Default::default()
        };
        let c = Completeness::default();
        let m = build(&input(&t, &c, "codex"));

        assert_eq!(m.reasoning_tokens.value, Some(175));
        assert_eq!(m.reasoning_tokens.display_kind(), DisplayKind::Exact);
    }

    #[test]
    fn a_mixed_task_reports_reasoning_as_a_floor_over_the_requests_that_have_it() {
        // Ten Claude requests and five Codex ones: the reasoning total covers
        // five of fifteen and must say so.
        let t = TaskTotals {
            requests: 15,
            reasoning: Some(175),
            reasoning_reported_by: 5,
            output_total: 2_000,
            ..Default::default()
        };
        let c = Completeness::default();
        let m = build(&input(&t, &c, "codex"));

        assert_eq!(m.reasoning_tokens.value, Some(175));
        assert!(matches!(
            m.reasoning_tokens.accuracy,
            aum_contract::Accuracy::Partial {
                measured: 5,
                total: 15,
                ..
            }
        ));
    }

    #[test]
    fn a_subscription_task_never_claims_an_actual_charge() {
        let t = totals();
        let c = Completeness::default();
        let m = build(&input(&t, &c, "claude_code"));

        assert_eq!(m.cost.actual_billed.value, None);
        assert!(matches!(
            m.cost.actual_billed.accuracy,
            aum_contract::Accuracy::Unavailable {
                reason: UnavailableReason::SubscriptionBilled { .. }
            }
        ));
    }

    #[test]
    fn latency_is_unavailable_and_says_what_would_make_it_available() {
        // The alternative — dividing output by the gap between timestamps —
        // yields a plausible, well-scaled, wrong number. It is not offered.
        let t = totals();
        let c = Completeness::default();
        let m = build(&input(&t, &c, "claude_code"));

        for metric in [
            &m.latency.p95_ms,
            &m.latency.average_ms,
            &m.latency.time_to_first_token_ms,
            &m.latency.output_tokens_per_sec,
        ] {
            assert_eq!(metric.value, None);
        }

        let aum_contract::Accuracy::Unavailable { reason } = &m.latency.p95_ms.accuracy else {
            panic!("expected unavailable")
        };
        let sentence = reason.sentence();
        assert!(sentence.contains("proxy") || sentence.contains("OpenTelemetry"));
        assert!(sentence.contains("Claude Code"));
    }

    #[test]
    fn unclassified_tokens_are_included_in_the_total() {
        // Codex's compaction calls. Real tokens, not attributable to a side.
        let t = TaskTotals {
            requests: 1,
            unclassified: 15_683,
            ..Default::default()
        };
        let c = Completeness::default();
        let m = build(&input(&t, &c, "codex"));

        assert_eq!(m.total_tokens.value, Some(15_683));
        assert_eq!(m.bands.unclassified, 15_683);
        // They are counted, but not attributed to either side.
        assert_eq!(m.bands.input_side_total(), 0);
        assert_eq!(m.bands.output_total, 0);
    }

    #[test]
    fn retries_stay_unavailable_where_the_agent_does_not_expose_them() {
        // Reporting 0 would make Codex look flawless when it is merely opaque.
        let t = totals();
        let c = Completeness::default();
        let mut i = input(&t, &c, "codex");
        i.retries = None;
        assert_eq!(build(&i).requests.retries, None);
    }
}

/// Cost a task from its per-model totals.
///
/// Costing has to happen per model and be summed: a task may span models whose
/// rates differ by an order of magnitude, so a single grand total cannot be
/// priced at all.
#[must_use]
pub fn cost_task(
    per_model: &[(Option<String>, TaskTotals)],
    table: &aum_pricing::PriceTable,
) -> Measured<Money> {
    let items: Vec<_> = per_model
        .iter()
        .map(|(model, totals)| (to_usage(totals), model.clone()))
        .collect();
    aum_pricing::cost_of_many(&items, table)
}

/// Rebuild a `TokenUsage` from stored columns.
///
/// The columns were written from a `TokenUsage` in the first place, so the
/// buckets are already disjoint and no provider semantics are re-applied here.
fn to_usage(t: &TaskTotals) -> aum_domain::TokenUsage {
    aum_domain::TokenUsage::from_bands(aum_contract::TokenBands {
        input_fresh: as_u64(t.input_fresh),
        cache_read: as_u64(t.cache_read),
        cache_write_5m: as_u64(t.cache_write_5m),
        cache_write_1h: as_u64(t.cache_write_1h),
        cache_write_unspecified: as_u64(t.cache_write_unspecified),
        output_total: as_u64(t.output_total),
        reasoning: t.reasoning.map(as_u64),
        unclassified: as_u64(t.unclassified),
    })
}

#[cfg(test)]
mod cost_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use aum_contract::DisplayKind;

    fn totals(input: i64, output: i64) -> TaskTotals {
        TaskTotals {
            requests: 1,
            input_fresh: input,
            output_total: output,
            ..Default::default()
        }
    }

    #[test]
    fn a_task_spanning_two_models_is_priced_per_model() {
        // A single grand total could not be priced at all: the rates differ by
        // an order of magnitude.
        let table = aum_pricing::PriceTable::from_entries(vec![
            aum_pricing::ModelPricing {
                version_id: "a".into(),
                model_id: "expensive".into(),
                rates: aum_pricing::Rates {
                    input_per_mtok: rust_decimal::Decimal::from(15),
                    output_per_mtok: rust_decimal::Decimal::from(75),
                    cache_read_per_mtok: rust_decimal::Decimal::from(2),
                    cache_write_5m_per_mtok: rust_decimal::Decimal::from(19),
                    cache_write_1h_per_mtok: rust_decimal::Decimal::from(30),
                },
                effective_from: "2026-01-01T00:00:00.000Z".into(),
                source: "seed".into(),
            },
            aum_pricing::ModelPricing {
                version_id: "b".into(),
                model_id: "cheap".into(),
                rates: aum_pricing::Rates {
                    input_per_mtok: rust_decimal::Decimal::from(1),
                    output_per_mtok: rust_decimal::Decimal::from(5),
                    cache_read_per_mtok: rust_decimal::Decimal::ZERO,
                    cache_write_5m_per_mtok: rust_decimal::Decimal::from(1),
                    cache_write_1h_per_mtok: rust_decimal::Decimal::from(2),
                },
                effective_from: "2026-01-01T00:00:00.000Z".into(),
                source: "seed".into(),
            },
        ]);

        let per_model = vec![
            (Some("expensive".to_owned()), totals(1_000_000, 0)),
            (Some("cheap".to_owned()), totals(1_000_000, 0)),
        ];
        let cost = cost_task(&per_model, &table);
        assert_eq!(cost.display_kind(), DisplayKind::Calculated);
        // 15 + 1, not 2 x either rate.
        assert_eq!(
            cost.value.unwrap().amount(),
            rust_decimal::Decimal::from(16)
        );
    }

    #[test]
    fn a_task_using_an_unpriced_model_reports_a_floor() {
        let table = aum_pricing::PriceTable::from_entries(vec![]);
        let per_model = vec![(Some("gpt-5.6-sol".to_owned()), totals(1_000, 100))];
        let cost = cost_task(&per_model, &table);
        assert_eq!(cost.value, None, "never zero for an unknown model");
        assert_eq!(cost.display_kind(), DisplayKind::Unavailable);
    }
}
