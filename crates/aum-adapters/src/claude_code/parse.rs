//! The Claude Code transcript parser.

use aum_domain::{AnthropicUsage, TokenUsage};
use serde::Deserialize;

use super::SOURCE_TRANSCRIPT;
use crate::{LineCtx, ParseOutcome, Signal, UsageSignal};

/// One transcript line.
///
/// Every field is optional and unknown keys are accepted. This is an
/// undocumented format belonging to a tool that ships weekly; a parser that
/// rejected an unfamiliar key would start losing data the first time a field
/// was added, and losing data here is invisible until a total is wrong.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    is_sidechain: Option<bool>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    attribution_agent: Option<String>,
    #[serde(default)]
    is_api_error_message: Option<bool>,
    #[serde(default)]
    retry_attempt: Option<u32>,
    #[serde(default)]
    max_retries: Option<u32>,
    /// `system` lines: `api_error` | `compact_boundary` | `stop_hook_summary`.
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    pre_tokens: Option<u64>,
    #[serde(default)]
    post_tokens: Option<u64>,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<AnthropicUsage>,
}

/// A locally generated failure marker, not a model response.
///
/// Verified shape: `model == "<synthetic>"`, all-zero usage, a bare-UUID message
/// id rather than `msg_…`, and `isApiErrorMessage: true`. It carries no tokens,
/// so it cannot corrupt token totals — but counting it as a request would
/// corrupt request counts and failure rates, which are exactly the columns a
/// benchmark compares.
const SYNTHETIC_MODEL: &str = "<synthetic>";

pub fn parse_line(ctx: &mut LineCtx, line: &[u8]) -> ParseOutcome {
    let parsed: Line = match serde_json::from_slice(line) {
        Ok(l) => l,
        Err(e) => {
            return ParseOutcome::Malformed {
                reason: format!("not valid JSON: {e}"),
            };
        }
    };

    let Some(session_id) = parsed.session_id.clone() else {
        // Without a session there is nothing to attribute to. Not an error —
        // several line types legitimately lack one.
        return ParseOutcome::Ignored;
    };

    let occurred_at = parsed
        .timestamp
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(chrono::Utc::now);

    let mut signals = Vec::new();

    if ctx.current_session.as_deref() != Some(session_id.as_str()) {
        ctx.current_session = Some(session_id.clone());
        signals.push(Signal::SessionOpened {
            session_id: session_id.clone(),
            cwd: parsed.cwd.clone(),
            app_version: parsed.version.clone(),
        });
    }

    match parsed.kind.as_deref() {
        Some("assistant") => {
            parse_assistant(ctx, &parsed, &session_id, occurred_at, line, &mut signals);
        }
        Some("system") => {
            parse_system(&parsed, &session_id, occurred_at, &mut signals);
        }
        _ => {}
    }

    if signals.is_empty() {
        ParseOutcome::Ignored
    } else {
        ParseOutcome::Signals(signals)
    }
}

fn parse_assistant(
    ctx: &mut LineCtx,
    parsed: &Line,
    session_id: &str,
    occurred_at: chrono::DateTime<chrono::Utc>,
    raw_line: &[u8],
    signals: &mut Vec<Signal>,
) {
    let Some(message) = parsed.message.as_ref() else {
        return;
    };

    let model = message.model.as_deref();

    // A synthetic line is a failure marker, not a request.
    if model == Some(SYNTHETIC_MODEL) || parsed.is_api_error_message == Some(true) {
        signals.push(Signal::RequestFailed {
            session_id: session_id.to_owned(),
            dedup_key: failure_key(parsed),
            occurred_at,
            detail: "the API call failed and produced no usable response".to_owned(),
        });
        return;
    }

    let Some(usage) = message.usage.as_ref() else {
        return;
    };

    if let Some(m) = model
        && ctx.current_model.as_deref() != Some(m)
    {
        ctx.current_model = Some(m.to_owned());
        signals.push(Signal::ModelDeclared {
            session_id: session_id.to_owned(),
            model_id: m.to_owned(),
        });
    }

    let normalized = match TokenUsage::from_anthropic(usage) {
        Ok(n) => {
            // The provider disagreed with itself, but the tokens are still real
            // and are still kept. Record it so the aggregate stops claiming to
            // be exact — and so a format change shows up as a rising count
            // rather than as quietly shifting numbers.
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

    let Some(dedup_key) = usage_key(parsed, message) else {
        signals.push(Signal::Anomaly {
            kind: "no_dedup_key".to_owned(),
            detail: "an assistant line carried usage but neither a requestId nor a message id, \
                     so repeated observations of it could not be recognised as the same request"
                .to_owned(),
        });
        return;
    };

    ctx.event_ordinal = ctx.event_ordinal.saturating_add(1);

    signals.push(Signal::Usage(Box::new(UsageSignal {
        session_id: session_id.to_owned(),
        dedup_key,
        model_id: model.map(str::to_owned),
        occurred_at,
        usage: normalized,
        measurement_source: SOURCE_TRANSCRIPT,
        request_kind: "turn",
        is_sidechain: parsed.is_sidechain.unwrap_or(false),
        agent_id: parsed.agent_id.clone(),
        agent_type: parsed.attribution_agent.clone(),
        // Only the usage object, not the whole line: the line contains prompt
        // and response text, and content capture is off by default.
        raw_json: serde_json::to_string(usage).ok().or_else(|| {
            debug_assert!(false, "usage object failed to re-serialize");
            let _ = raw_line;
            None
        }),
    })));
}

fn parse_system(
    parsed: &Line,
    session_id: &str,
    occurred_at: chrono::DateTime<chrono::Utc>,
    signals: &mut Vec<Signal>,
) {
    match parsed.subtype.as_deref() {
        Some("api_error") => {
            // One attempt, not one failure. A verified real chain is 8 attempts
            // for a single incident; counting lines would overstate the failure
            // rate by 7.5x.
            signals.push(Signal::RetryAttempt {
                session_id: session_id.to_owned(),
                attempt: parsed.retry_attempt.unwrap_or(1),
                max_attempts: parsed.max_retries,
                occurred_at,
            });
        }
        Some("compact_boundary") => {
            // Context sizes, not billing. `preTokens` on a real boundary is
            // ~999,000; treating it as usage adds a million phantom tokens.
            signals.push(Signal::ContextCompacted {
                session_id: session_id.to_owned(),
                pre_tokens: parsed.pre_tokens,
                post_tokens: parsed.post_tokens,
            });
        }
        _ => {}
    }
}

/// The identity of a request.
///
/// `requestId` alone is not quite enough: it is per HTTP call, and pairing it
/// with the message id keeps the key meaningful if a single call ever yields
/// more than one message. Both are present on every real assistant line
/// carrying usage.
fn usage_key(parsed: &Line, message: &Message) -> Option<String> {
    match (parsed.request_id.as_deref(), message.id.as_deref()) {
        (Some(req), Some(msg)) => Some(format!("{req}:{msg}")),
        (Some(req), None) => Some(req.to_owned()),
        (None, Some(msg)) => Some(msg.to_owned()),
        (None, None) => None,
    }
}

/// Failures have no request id, so fall back to the line's own uuid — which is
/// stable across re-reads of the same file.
fn failure_key(parsed: &Line) -> String {
    parsed
        .request_id
        .clone()
        .or_else(|| parsed.uuid.clone())
        .unwrap_or_else(|| "unknown-failure".to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::UsageAdapter;
    use crate::claude_code::ClaudeCodeAdapter;

    fn parse(line: &str) -> (LineCtx, ParseOutcome) {
        let mut ctx = LineCtx::default();
        let outcome = parse_line(&mut ctx, line.as_bytes());
        (ctx, outcome)
    }

    fn usage_signals(outcome: &ParseOutcome) -> Vec<&UsageSignal> {
        match outcome {
            ParseOutcome::Signals(s) => s
                .iter()
                .filter_map(|x| match x {
                    Signal::Usage(u) => Some(u.as_ref()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Shaped exactly like a real assistant line, with all text removed.
    fn assistant_line(request_id: &str, message_id: &str, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"11111111-1111-1111-1111-111111111111",
                 "uuid":"22222222-2222-2222-2222-222222222222",
                 "parentUuid":"33333333-3333-3333-3333-333333333333",
                 "timestamp":"2026-08-12T19:10:15.774Z","version":"2.1.227",
                 "gitBranch":"main","cwd":"/redacted","userType":"external",
                 "isSidechain":false,"entrypoint":"cli","requestId":"{request_id}",
                 "effort":"high",
                 "message":{{"id":"{message_id}","type":"message","role":"assistant",
                   "model":"claude-opus-5","stop_reason":"tool_use",
                   "content":[{{"type":"tool_use"}}],
                   "usage":{{"input_tokens":2,"cache_creation_input_tokens":23610,
                     "cache_read_input_tokens":30885,"output_tokens":{output},
                     "server_tool_use":{{"web_search_requests":0,"web_fetch_requests":0}},
                     "service_tier":"standard",
                     "cache_creation":{{"ephemeral_1h_input_tokens":23610,"ephemeral_5m_input_tokens":0}},
                     "inference_geo":"not_available",
                     "iterations":[{{"input_tokens":2,"output_tokens":{output},
                       "cache_read_input_tokens":30885,"cache_creation_input_tokens":23610,
                       "type":"message"}}]}}}}}}"#
        )
        .replace('\n', "")
    }

    #[test]
    fn extracts_the_provider_usage_object_verbatim() {
        let (_, outcome) = parse(&assistant_line("req_1", "msg_1", 4452));
        let usage = usage_signals(&outcome);
        assert_eq!(usage.len(), 1);

        let u = &usage.first().unwrap().usage;
        assert_eq!(u.input_fresh(), 2);
        assert_eq!(u.cache_read(), 30_885);
        assert_eq!(u.cache_write_1h(), 23_610);
        assert_eq!(u.output_total(), 4_452);
        // Claude Code reports no reasoning count.
        assert_eq!(u.reasoning(), None);
    }

    #[test]
    fn the_iterations_array_is_not_added_to_the_top_level() {
        // `iterations` is a breakdown of the same message. Verified across a
        // full session: summing it with the top level doubles every request.
        let (_, outcome) = parse(&assistant_line("req_1", "msg_1", 4452));
        assert_eq!(
            usage_signals(&outcome)
                .first()
                .unwrap()
                .usage
                .output_total(),
            4_452
        );
    }

    #[test]
    fn the_fanout_lines_of_one_response_share_one_dedup_key() {
        // The 2.12x bug. Eight lines, one response: same requestId and message
        // id, different uuids and timestamps.
        let keys: Vec<String> = (0..8)
            .map(|_| {
                let (_, outcome) = parse(&assistant_line("req_1", "msg_1", 1454));
                usage_signals(&outcome).first().unwrap().dedup_key.clone()
            })
            .collect();

        assert_eq!(keys.len(), 8);
        assert!(
            keys.windows(2).all(|w| w[0] == w[1]),
            "all eight must collapse to one key"
        );
    }

    #[test]
    fn distinct_responses_get_distinct_keys() {
        let (_, a) = parse(&assistant_line("req_1", "msg_1", 10));
        let (_, b) = parse(&assistant_line("req_2", "msg_2", 20));
        assert_ne!(
            usage_signals(&a).first().unwrap().dedup_key,
            usage_signals(&b).first().unwrap().dedup_key
        );
    }

    #[test]
    fn a_synthetic_line_is_a_failure_and_not_a_request() {
        // Zero usage, so it cannot corrupt token totals — but counting it as a
        // request would corrupt request counts and failure rates.
        let line = r#"{"type":"assistant","sessionId":"s1","uuid":"u1",
          "timestamp":"2026-08-12T19:10:15.774Z","isApiErrorMessage":true,
          "message":{"id":"e5f6","model":"<synthetic>","usage":{"input_tokens":0,
            "output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#
            .replace('\n', "");

        let (_, outcome) = parse(&line);
        assert!(
            usage_signals(&outcome).is_empty(),
            "must not count as usage"
        );
        let ParseOutcome::Signals(signals) = &outcome else {
            panic!("expected signals, got {outcome:?}")
        };
        assert!(
            signals
                .iter()
                .any(|s| matches!(s, Signal::RequestFailed { .. }))
        );
    }

    #[test]
    fn compact_boundary_token_counts_never_reach_a_total() {
        // preTokens on a real boundary is ~999,000. Treating it as usage adds a
        // million phantom tokens to the session.
        let line = r#"{"type":"system","subtype":"compact_boundary","sessionId":"s1",
          "timestamp":"2026-08-12T19:10:15.774Z","preTokens":998935,"postTokens":10739}"#
            .replace('\n', "");

        let (_, outcome) = parse(&line);
        assert!(usage_signals(&outcome).is_empty());
        let ParseOutcome::Signals(signals) = &outcome else {
            panic!("expected signals")
        };
        assert!(signals.iter().any(|s| matches!(
            s,
            Signal::ContextCompacted {
                pre_tokens: Some(998_935),
                ..
            }
        )));
    }

    #[test]
    fn api_error_lines_are_attempts_rather_than_failures() {
        // A verified real chain is 8 attempts for one incident; treating each
        // line as a failure overstates the rate by 7.5x.
        let line = r#"{"type":"system","subtype":"api_error","level":"error","sessionId":"s1",
          "timestamp":"2026-08-12T19:10:15.774Z","retryAttempt":3,"maxRetries":8}"#
            .replace('\n', "");

        let (_, outcome) = parse(&line);
        let ParseOutcome::Signals(signals) = &outcome else {
            panic!("expected signals")
        };
        assert!(signals.iter().any(|s| matches!(
            s,
            Signal::RetryAttempt {
                attempt: 3,
                max_attempts: Some(8),
                ..
            }
        )));
        assert!(
            !signals
                .iter()
                .any(|s| matches!(s, Signal::RequestFailed { .. }))
        );
    }

    #[test]
    fn a_subagent_line_attributes_to_the_parent_session() {
        // Sub-agent transcripts live in separate files but carry the parent's
        // session id — which is why attribution keys on the field, not the path.
        // Missing these loses 76% of requests on a real session.
        let line = r#"{"type":"assistant","sessionId":"parent-session","uuid":"u1",
          "timestamp":"2026-08-12T19:10:15.774Z","isSidechain":true,
          "agentId":"agent-7","attributionAgent":"Explore","requestId":"req_9",
          "message":{"id":"msg_9","model":"claude-haiku-4-5-20251001",
            "usage":{"input_tokens":100,"output_tokens":50,
              "cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#
            .replace('\n', "");

        let (_, outcome) = parse(&line);
        let u = usage_signals(&outcome);
        let signal = u.first().unwrap();
        assert_eq!(signal.session_id, "parent-session");
        assert!(signal.is_sidechain);
        assert_eq!(signal.agent_type.as_deref(), Some("Explore"));
        assert_eq!(
            signal.model_id.as_deref(),
            Some("claude-haiku-4-5-20251001")
        );
    }

    #[test]
    fn a_user_line_carries_no_usage() {
        let line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-08-12T19:10:15.774Z",
          "message":{"role":"user","content":"[redacted]"}}"#
            .replace('\n', "");
        assert!(usage_signals(&parse(&line).1).is_empty());
    }

    #[test]
    fn malformed_json_is_reported_rather_than_silently_dropped() {
        let (_, outcome) = parse("{not json");
        assert!(matches!(outcome, ParseOutcome::Malformed { .. }));
    }

    #[test]
    fn unknown_fields_do_not_break_parsing() {
        // The format belongs to a tool that ships weekly. Rejecting new keys
        // would start losing data silently.
        let mut line = assistant_line("req_1", "msg_1", 100);
        line = line.replace(
            r#""type":"assistant","#,
            r#""type":"assistant","somethingBrandNew":{"nested":true},"#,
        );
        assert_eq!(usage_signals(&parse(&line).1).len(), 1);
    }

    #[test]
    fn the_stored_audit_trail_is_the_usage_object_not_the_conversation() {
        // Content capture is off by default; the raw JSON kept for auditing must
        // not smuggle prompt or response text into the database.
        let (_, outcome) = parse(&assistant_line("req_1", "msg_1", 100));
        let raw = usage_signals(&outcome)
            .first()
            .unwrap()
            .raw_json
            .clone()
            .unwrap();
        assert!(raw.contains("input_tokens"), "the counts must be auditable");
        // Note `server_tool_use` legitimately contains the substring `tool_use`,
        // so these check for the conversation-carrying keys specifically.
        for forbidden in ["\"content\"", "\"role\"", "\"stop_reason\"", "\"cwd\""] {
            assert!(
                !raw.contains(forbidden),
                "audit trail leaked {forbidden}: {raw}"
            );
        }
    }

    #[test]
    fn the_prefilter_accepts_usage_lines_and_rejects_ordinary_ones() {
        let adapter = ClaudeCodeAdapter;
        assert!(adapter.is_candidate_line(br#"{"type":"assistant","message":{"usage":{}}}"#));
        assert!(adapter.is_candidate_line(br#"{"type":"system","subtype":"api_error"}"#));
        assert!(!adapter.is_candidate_line(br#"{"type":"user","message":{"role":"user"}}"#));
    }

    #[test]
    fn a_session_is_announced_once_rather_than_on_every_line() {
        let mut ctx = LineCtx::default();
        let line = assistant_line("req_1", "msg_1", 100);

        let first = parse_line(&mut ctx, line.as_bytes());
        let second = parse_line(&mut ctx, line.as_bytes());

        let count = |o: &ParseOutcome| match o {
            ParseOutcome::Signals(s) => s
                .iter()
                .filter(|x| matches!(x, Signal::SessionOpened { .. }))
                .count(),
            _ => 0,
        };
        assert_eq!(count(&first), 1);
        assert_eq!(count(&second), 0);
    }
}
