//! The Codex rollout parser.

use aum_domain::{OpenAiUsage, TokenUsage};
use serde::Deserialize;

use super::reconcile::Reconciled;
use super::{SOURCE_CUMULATIVE_DELTA, SOURCE_OFF_LEDGER};
use crate::{LineCtx, ParseOutcome, Signal, UsageSignal};

#[derive(Debug, Deserialize)]
struct Line {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    payload: Option<Payload>,
}

#[derive(Debug, Deserialize)]
struct Payload {
    #[serde(default, rename = "type")]
    kind: Option<String>,

    // session_meta
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    cli_version: Option<String>,

    // turn_context
    #[serde(default)]
    model: Option<String>,

    // token_count
    ///
    /// Genuinely nullable: 18 events in the corpus carry rate-limit status and
    /// no token data at all. Treating that as a zero-token turn would invent
    /// requests that never happened.
    #[serde(default)]
    info: Option<TokenInfo>,
}

#[derive(Debug, Deserialize)]
struct TokenInfo {
    #[serde(default)]
    total_token_usage: Option<OpenAiUsage>,
    #[serde(default)]
    last_token_usage: Option<OpenAiUsage>,
}

pub fn parse_line(ctx: &mut LineCtx, line: &[u8]) -> ParseOutcome {
    let parsed: Line = match serde_json::from_slice(line) {
        Ok(l) => l,
        Err(e) => {
            return ParseOutcome::Malformed {
                reason: format!("not valid JSON: {e}"),
            };
        }
    };

    let Some(payload) = parsed.payload else {
        return ParseOutcome::Ignored;
    };

    let occurred_at = parsed
        .timestamp
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(chrono::Utc::now);

    let mut signals = Vec::new();

    match parsed.kind.as_deref() {
        Some("session_meta") => {
            if let Some(id) = payload.id.clone() {
                ctx.current_session = Some(id.clone());
                signals.push(Signal::SessionOpened {
                    session_id: id,
                    cwd: payload.cwd.clone(),
                    app_version: payload.cli_version.clone(),
                });
            }
        }

        Some("turn_context") => {
            // The model applies to subsequent turns and is not repeated on the
            // usage events themselves.
            if let Some(model) = payload.model.clone()
                && ctx.current_model.as_deref() != Some(model.as_str())
            {
                ctx.current_model = Some(model.clone());
                if let Some(session_id) = ctx.current_session.clone() {
                    signals.push(Signal::ModelDeclared {
                        session_id,
                        model_id: model,
                    });
                }
            }
        }

        Some("event_msg") => match payload.kind.as_deref() {
            Some("token_count") => {
                parse_token_count(ctx, payload.info.as_ref(), occurred_at, &mut signals);
            }
            Some("context_compacted") => {
                if let Some(session_id) = ctx.current_session.clone() {
                    signals.push(Signal::ContextCompacted {
                        session_id,
                        pre_tokens: None,
                        post_tokens: None,
                    });
                }
            }
            _ => {}
        },

        _ => {}
    }

    if signals.is_empty() {
        ParseOutcome::Ignored
    } else {
        ParseOutcome::Signals(signals)
    }
}

fn parse_token_count(
    ctx: &mut LineCtx,
    info: Option<&TokenInfo>,
    occurred_at: chrono::DateTime<chrono::Utc>,
    signals: &mut Vec<Signal>,
) {
    let Some(session_id) = ctx.current_session.clone() else {
        // A rollout always opens with `session_meta`; reaching usage without one
        // means we started mid-file and cannot attribute this turn to anything.
        signals.push(Signal::Anomaly {
            kind: "usage_before_session".to_owned(),
            detail: "a token_count event arrived before any session_meta, so it cannot be \
                     attributed"
                .to_owned(),
        });
        return;
    };

    let cumulative = info.and_then(|i| i.total_token_usage);
    let last = info.and_then(|i| i.last_token_usage);

    let mut watermark = crate::codex::Watermark {
        cumulative: ctx.previous_cumulative,
        last: ctx.previous_last,
        ..Default::default()
    };
    let outcome = watermark.observe(cumulative, last);
    ctx.previous_cumulative = watermark.cumulative;
    ctx.previous_last = watermark.last;

    let (native, source, kind) = match outcome {
        // Nothing happened, or it happened already. Emitting anything here is
        // what over-counts a session by ~0.89%.
        Reconciled::Replay | Reconciled::NoTokenData => return,

        Reconciled::Turn { usage } => (usage, SOURCE_CUMULATIVE_DELTA, "turn"),

        Reconciled::OffLedger { usage } => (usage, SOURCE_OFF_LEDGER, "off_ledger"),

        Reconciled::EpochReset {
            usage,
            from_total,
            to_total,
        } => {
            signals.push(Signal::Anomaly {
                kind: "cumulative_rebased".to_owned(),
                detail: format!(
                    "the session's running total went backwards, {from_total} -> {to_total}; \
                     starting a new epoch rather than emitting a negative delta"
                ),
            });
            (usage, SOURCE_CUMULATIVE_DELTA, "turn")
        }
    };

    // The provider's own arithmetic, checked but not trusted blindly: one event
    // in 21,784 fails it. Skipped for off-ledger rows, where the mismatch is the
    // known shape of the data rather than a surprise, and is handled below.
    if kind != "off_ledger"
        && let Some(discrepancy) = TokenUsage::check_openai_total(&native)
    {
        signals.push(Signal::Anomaly {
            kind: "provider_self_inconsistent".to_owned(),
            detail: discrepancy.to_string(),
        });
    }

    // Compaction calls report a total with input and output both zero: the
    // tokens were spent, but the provider does not say on which side. Recording
    // zero loses them (~3M across this machine's history); attributing them to
    // one side would misprice by up to 8x. Count them, price them nowhere.
    let total = native.total_tokens.unwrap_or(0);
    let classified = native
        .input_tokens
        .unwrap_or(0)
        .saturating_add(native.output_tokens.unwrap_or(0));
    if kind == "off_ledger" && classified == 0 && total > 0 {
        ctx.event_ordinal = ctx.event_ordinal.saturating_add(1);
        signals.push(Signal::Usage(Box::new(UsageSignal {
            session_id,
            dedup_key: format!("{}:{kind}", ctx.event_ordinal),
            model_id: ctx.current_model.clone(),
            occurred_at,
            usage: TokenUsage::unclassified_only(total),
            measurement_source: source,
            request_kind: kind,
            is_sidechain: false,
            agent_id: None,
            agent_type: None,
            raw_json: serde_json::to_string(&native).ok(),
        })));
        return;
    }

    let normalized = match TokenUsage::from_openai(&native) {
        Ok(n) => {
            if let Some(discrepancy) = n.discrepancy {
                signals.push(Signal::Anomaly {
                    kind: "provider_self_inconsistent".to_owned(),
                    detail: discrepancy.to_string(),
                });
            }
            n.usage
        }
        Err(e) => {
            signals.push(Signal::Anomaly {
                kind: "normalize_failed".to_owned(),
                detail: e.to_string(),
            });
            return;
        }
    };

    ctx.event_ordinal = ctx.event_ordinal.saturating_add(1);

    signals.push(Signal::Usage(Box::new(UsageSignal {
        session_id,
        // Codex supplies no per-request id, so identity is the event's position
        // in an append-only file. Stable across re-reads, and distinct for the
        // off-ledger call that shares a position with an ordinary turn.
        dedup_key: format!("{}:{kind}", ctx.event_ordinal),
        model_id: ctx.current_model.clone(),
        occurred_at,
        usage: normalized,
        measurement_source: source,
        request_kind: kind,
        is_sidechain: false,
        agent_id: None,
        agent_type: None,
        raw_json: serde_json::to_string(&native).ok(),
    })));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::UsageAdapter;
    use crate::codex::CodexAdapter;

    fn session_meta() -> String {
        r#"{"timestamp":"2026-05-14T12:20:25.149Z","type":"session_meta","payload":{
            "id":"019e266e-53bd-73f0-ad31-a61e8a6000c5","cwd":"/redacted",
            "originator":"Codex Desktop","cli_version":"0.130.0-alpha.5","source":"vscode"}}"#
            .replace('\n', "")
    }

    fn turn_context(model: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-05-14T12:20:26.000Z","type":"turn_context","payload":{{
              "turn_id":"t1","cwd":"/redacted","model":"{model}","effort":"xhigh"}}}}"#
        )
        .replace('\n', "")
    }

    fn token_count(input: u64, cached: u64, output: u64, reasoning: u64, last: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-05-14T12:20:30.000Z","type":"event_msg","payload":{{
              "type":"token_count",
              "info":{{"total_token_usage":{{"input_tokens":{input},
                 "cached_input_tokens":{cached},"output_tokens":{output},
                 "reasoning_output_tokens":{reasoning},"total_tokens":{}}},
               "last_token_usage":{last},"model_context_window":258400}},
              "rate_limits":{{"limit_id":"codex","plan_type":"plus"}}}}}}"#,
            input + output
        )
        .replace('\n', "")
    }

    fn usage_json(input: u64, cached: u64, output: u64, reasoning: u64) -> String {
        format!(
            r#"{{"input_tokens":{input},"cached_input_tokens":{cached},
              "output_tokens":{output},"reasoning_output_tokens":{reasoning},
              "total_tokens":{}}}"#,
            input + output
        )
        .replace('\n', "")
    }

    fn run(lines: &[String]) -> (LineCtx, Vec<Signal>) {
        let mut ctx = LineCtx::default();
        let mut all = Vec::new();
        for line in lines {
            if let ParseOutcome::Signals(s) = parse_line(&mut ctx, line.as_bytes()) {
                all.extend(s);
            }
        }
        (ctx, all)
    }

    fn usages(signals: &[Signal]) -> Vec<&UsageSignal> {
        signals
            .iter()
            .filter_map(|s| match s {
                Signal::Usage(u) => Some(u.as_ref()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_turn_reports_the_delta_not_the_running_total() {
        let (_, signals) = run(&[
            session_meta(),
            turn_context("gpt-5.5"),
            token_count(1_000, 0, 100, 0, &usage_json(1_000, 0, 100, 0)),
            token_count(3_000, 500, 250, 50, &usage_json(2_000, 500, 150, 50)),
        ]);

        let u = usages(&signals);
        assert_eq!(u.len(), 2);
        // Second turn: 3000-1000 input, of which 500 cached, and 250-100 output.
        let second = u.get(1).unwrap();
        assert_eq!(second.usage.input_fresh(), 1_500);
        assert_eq!(second.usage.cache_read(), 500);
        assert_eq!(second.usage.output_total(), 150);
    }

    #[test]
    fn reasoning_tokens_are_reported_and_stay_inside_output() {
        let (_, signals) = run(&[
            session_meta(),
            turn_context("gpt-5.5"),
            token_count(
                18_517,
                12_160,
                328,
                106,
                &usage_json(18_517, 12_160, 328, 106),
            ),
        ]);

        let u = usages(&signals);
        let usage = &u.first().unwrap().usage;
        // Codex reports reasoning; Claude Code does not. This asymmetry must
        // survive into the UI as a real difference.
        assert_eq!(usage.reasoning(), Some(106));
        assert_eq!(usage.output_total(), 328);
        assert!(usage.reasoning().unwrap() <= usage.output_total());
    }

    #[test]
    fn cached_input_is_subtracted_so_it_is_not_billed_twice() {
        let (_, signals) = run(&[
            session_meta(),
            turn_context("gpt-5.5"),
            token_count(
                16_210,
                11_008,
                164,
                35,
                &usage_json(16_210, 11_008, 164, 35),
            ),
        ]);

        let usage = &usages(&signals).first().unwrap().usage;
        assert_eq!(usage.input_fresh(), 5_202);
        assert_eq!(usage.cache_read(), 11_008);
        // Charging input in full and cached again would overstate this by 145%.
        assert_eq!(usage.input_side_total(), 16_210);
    }

    #[test]
    fn a_verbatim_duplicate_event_produces_no_second_request() {
        let event = token_count(1_000, 0, 100, 0, &usage_json(1_000, 0, 100, 0));
        let (_, signals) = run(&[
            session_meta(),
            turn_context("gpt-5.5"),
            event.clone(),
            event,
        ]);
        assert_eq!(
            usages(&signals).len(),
            1,
            "the duplicate must contribute nothing"
        );
    }

    #[test]
    fn a_compaction_call_the_ledger_omits_is_still_recorded() {
        let cum = token_count(24_873_976, 0, 0, 0, &usage_json(210_476, 0, 0, 0));
        // Same cumulative, different `last`: a billed call the provider excludes.
        let off_ledger = token_count(24_873_976, 0, 0, 0, &usage_json(15_683, 0, 0, 0));

        let (_, signals) = run(&[session_meta(), turn_context("gpt-5.5"), cum, off_ledger]);
        let u = usages(&signals);
        assert_eq!(u.len(), 2);

        let second = u.get(1).unwrap();
        assert_eq!(second.request_kind, "off_ledger");
        assert_eq!(second.measurement_source, SOURCE_OFF_LEDGER);
        assert_eq!(second.usage.input_fresh(), 15_683);
    }

    #[test]
    fn the_model_comes_from_turn_context_and_is_carried_forward() {
        let (_, signals) = run(&[
            session_meta(),
            turn_context("gpt-5.6-sol"),
            token_count(100, 0, 10, 0, &usage_json(100, 0, 10, 0)),
        ]);
        assert_eq!(
            usages(&signals).first().unwrap().model_id.as_deref(),
            Some("gpt-5.6-sol")
        );
    }

    #[test]
    fn usage_before_any_turn_context_has_no_model_rather_than_a_guessed_one() {
        // A tailer resuming mid-file genuinely does not know the model, so cost
        // must come out unavailable instead of being attributed to a default.
        let (_, signals) = run(&[
            session_meta(),
            token_count(100, 0, 10, 0, &usage_json(100, 0, 10, 0)),
        ]);
        assert_eq!(usages(&signals).first().unwrap().model_id, None);
    }

    #[test]
    fn an_event_with_null_info_is_not_a_request() {
        // 18 real events look exactly like this: rate limits, no tokens.
        let line = r#"{"timestamp":"2026-05-14T12:20:30.000Z","type":"event_msg","payload":{
            "type":"token_count","info":null,
            "rate_limits":{"limit_id":"codex","plan_type":"plus"}}}"#
            .replace('\n', "");

        let (_, signals) = run(&[session_meta(), turn_context("gpt-5.5"), line]);
        assert!(usages(&signals).is_empty());
    }

    #[test]
    fn a_rebased_counter_is_flagged_and_never_yields_a_negative_delta() {
        let (_, signals) = run(&[
            session_meta(),
            turn_context("gpt-5.5"),
            token_count(26_836_493, 0, 0, 0, &usage_json(100, 0, 0, 0)),
            token_count(353_400, 0, 0, 0, &usage_json(400, 0, 0, 0)),
        ]);

        assert!(
            signals
                .iter()
                .any(|s| matches!(s, Signal::Anomaly { kind, .. } if kind == "cumulative_rebased"))
        );
        let u = usages(&signals);
        assert_eq!(u.get(1).unwrap().usage.input_fresh(), 400);
    }

    #[test]
    fn every_usage_row_gets_a_distinct_key() {
        let (_, signals) = run(&[
            session_meta(),
            turn_context("gpt-5.5"),
            token_count(1_000, 0, 100, 0, &usage_json(1_000, 0, 100, 0)),
            token_count(2_000, 0, 200, 0, &usage_json(1_000, 0, 100, 0)),
            token_count(3_000, 0, 300, 0, &usage_json(1_000, 0, 100, 0)),
        ]);

        let keys: Vec<&str> = usages(&signals)
            .iter()
            .map(|u| u.dedup_key.as_str())
            .collect();
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(keys.len(), 3);
        assert_eq!(unique.len(), 3, "keys must not collide: {keys:?}");
    }

    #[test]
    fn the_prefilter_rejects_the_bulk_of_a_rollout() {
        let adapter = CodexAdapter;
        assert!(
            adapter.is_candidate_line(br#"{"type":"event_msg","payload":{"type":"token_count"}}"#)
        );
        assert!(adapter.is_candidate_line(br#"{"type":"turn_context","payload":{}}"#));
        assert!(adapter.is_candidate_line(br#"{"type":"session_meta","payload":{}}"#));
        // Tool output dominates these files and must be skipped before parsing.
        assert!(
            !adapter.is_candidate_line(br#"{"type":"response_item","payload":{"type":"message"}}"#)
        );
    }

    #[test]
    fn malformed_json_is_reported_rather_than_dropped() {
        let mut ctx = LineCtx::default();
        assert!(matches!(
            parse_line(&mut ctx, b"{not json"),
            ParseOutcome::Malformed { .. }
        ));
    }
}

#[cfg(test)]
mod unclassified_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// Codex's compaction calls report a total with input and output at zero.
    /// Recording that as zero loses ~3M tokens across this machine's history;
    /// splitting it by guess would misprice by up to eight times, since input
    /// and output rates differ by roughly that much.
    #[test]
    fn a_compaction_call_with_only_a_total_keeps_its_tokens_unclassified() {
        let mut ctx = LineCtx::default();
        let meta = r#"{"type":"session_meta","payload":{"id":"s1","cwd":"/x"}}"#;
        let first = r#"{"type":"event_msg","payload":{"type":"token_count","info":{
            "total_token_usage":{"input_tokens":1000,"output_tokens":100,"total_tokens":1100},
            "last_token_usage":{"input_tokens":1000,"output_tokens":100,"total_tokens":1100}}}}"#;
        // Same cumulative, and a `last` shaped exactly as the real data is.
        let off = r#"{"type":"event_msg","payload":{"type":"token_count","info":{
            "total_token_usage":{"input_tokens":1000,"output_tokens":100,"total_tokens":1100},
            "last_token_usage":{"input_tokens":0,"output_tokens":0,"total_tokens":15683}}}}"#;

        parse_line(&mut ctx, meta.replace('\n', "").as_bytes());
        parse_line(&mut ctx, first.replace('\n', "").as_bytes());
        let outcome = parse_line(&mut ctx, off.replace('\n', "").as_bytes());

        let ParseOutcome::Signals(signals) = outcome else {
            panic!("expected the compaction call to be recorded, got {outcome:?}")
        };
        let usage = signals
            .iter()
            .find_map(|s| match s {
                Signal::Usage(u) => Some(u.as_ref()),
                _ => None,
            })
            .expect("the compaction call must produce a usage row");

        assert_eq!(
            usage.usage.unclassified(),
            15_683,
            "the tokens must survive"
        );
        assert_eq!(usage.usage.grand_total(), 15_683);
        // Not attributed to either side, because the provider did not say.
        assert_eq!(usage.usage.input_side_total(), 0);
        assert_eq!(usage.usage.output_total(), 0);
        assert_eq!(usage.request_kind, "off_ledger");
    }
}
