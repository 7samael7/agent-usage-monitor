//! The Copilot OpenTelemetry export parser.
//!
//! One line is one OTLP/JSON envelope. The numbers live in span attributes,
//! whose names are the OpenTelemetry `gen_ai` semantic conventions.

use aum_contract::TokenBands;
use aum_domain::TokenUsage;
use serde::Deserialize;

use super::{ADAPTER_ID, SOURCE_OTEL_SPAN};
use crate::{LineCtx, ParseOutcome, Signal, UsageSignal};

/// Session totals. Never counted — see the module docs.
const SPAN_SESSION: &str = "invoke_agent";
/// One model call. The only thing counted.
const SPAN_CALL: &str = "chat";

const ATTR_INPUT: &str = "gen_ai.usage.input_tokens";
const ATTR_OUTPUT: &str = "gen_ai.usage.output_tokens";
const ATTR_CACHE_READ: &str = "gen_ai.usage.cache_read.input_tokens";
const ATTR_CACHE_WRITE: &str = "gen_ai.usage.cache_creation.input_tokens";
const ATTR_REASONING: &str = "gen_ai.usage.reasoning.output_tokens";
const ATTR_SESSION: &str = "gen_ai.conversation.id";
const ATTR_MODEL_RESPONSE: &str = "gen_ai.response.model";
const ATTR_MODEL_REQUEST: &str = "gen_ai.request.model";

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default, rename = "resourceSpans")]
    resource_spans: Vec<ResourceSpans>,
    /// Some exporters write a bare span per line rather than an envelope.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    attributes: Vec<Attribute>,
    #[serde(default, rename = "spanId")]
    span_id: Option<String>,
    #[serde(default, rename = "traceId")]
    trace_id: Option<String>,
    #[serde(default, rename = "endTimeUnixNano")]
    end_time: Option<Scalar>,
    #[serde(default, rename = "startTimeUnixNano")]
    start_time: Option<Scalar>,
}

#[derive(Debug, Deserialize)]
struct ResourceSpans {
    #[serde(default, rename = "scopeSpans")]
    scope_spans: Vec<ScopeSpans>,
}

#[derive(Debug, Deserialize)]
struct ScopeSpans {
    #[serde(default)]
    spans: Vec<Span>,
}

#[derive(Debug, Deserialize)]
struct Span {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    attributes: Vec<Attribute>,
    #[serde(default, rename = "spanId")]
    span_id: Option<String>,
    #[serde(default, rename = "traceId")]
    trace_id: Option<String>,
    #[serde(default, rename = "endTimeUnixNano")]
    end_time: Option<Scalar>,
    #[serde(default, rename = "startTimeUnixNano")]
    start_time: Option<Scalar>,
}

#[derive(Debug, Deserialize)]
struct Attribute {
    key: String,
    #[serde(default)]
    value: Option<AttrValue>,
}

/// OTLP/JSON writes 64-bit integers as **strings**, because that is what
/// proto3 JSON mandates — but exporters differ, and some emit a bare number.
/// Reading only one of the two shapes loses every count from the other.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Scalar {
    Str(String),
    Num(serde_json::Number),
}

impl Scalar {
    fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Str(s) => s.parse().ok(),
            Self::Num(n) => n.as_u64(),
        }
    }

    fn as_string(&self) -> String {
        match self {
            Self::Str(s) => s.clone(),
            Self::Num(n) => n.to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AttrValue {
    #[serde(default, rename = "intValue")]
    int_value: Option<Scalar>,
    #[serde(default, rename = "stringValue")]
    string_value: Option<String>,
    #[serde(default, rename = "doubleValue")]
    double_value: Option<f64>,
}

/// A span's attributes, reduced to the handful that are measurements.
///
/// Deliberately not the whole attribute list: a `chat` span can carry prompt
/// and completion content, and this struct is what reaches the audit trail.
#[derive(Debug, Default)]
struct Usage {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    /// `None` means the attribute was absent, which is not the same as a
    /// reported zero and must not become one.
    reasoning: Option<u64>,
    session: Option<String>,
    model: Option<String>,
}

fn collect(attributes: &[Attribute]) -> Usage {
    let mut out = Usage::default();
    let mut request_model = None;
    for attr in attributes {
        let Some(value) = attr.value.as_ref() else {
            continue;
        };
        let int = value
            .int_value
            .as_ref()
            .and_then(Scalar::as_u64)
            .or_else(|| {
                // A count that arrived as a double is still a count, but only if it
                // is a whole non-negative number; anything else is not a token.
                value
                    .double_value
                    .filter(|d| d.is_finite() && *d >= 0.0 && d.fract() == 0.0)
                    .map(|d| d as u64)
            });
        let text = value
            .string_value
            .clone()
            .or_else(|| value.int_value.as_ref().map(Scalar::as_string));

        match attr.key.as_str() {
            ATTR_INPUT => out.input = int,
            ATTR_OUTPUT => out.output = int,
            ATTR_CACHE_READ => out.cache_read = int,
            ATTR_CACHE_WRITE => out.cache_write = int,
            ATTR_REASONING => out.reasoning = int,
            ATTR_SESSION => out.session = text,
            ATTR_MODEL_RESPONSE => out.model = text,
            ATTR_MODEL_REQUEST => request_model = text,
            _ => {}
        }
    }
    // The model that answered, where it is known; the one asked for otherwise.
    // They differ when the service routes a request elsewhere, and the answer
    // is what was billed.
    out.model = out.model.or(request_model);
    out
}

fn nanos_to_utc(scalar: Option<&Scalar>) -> Option<chrono::DateTime<chrono::Utc>> {
    let nanos = scalar?.as_u64()?;
    let secs = i64::try_from(nanos / 1_000_000_000).ok()?;
    let sub = u32::try_from(nanos % 1_000_000_000).ok()?;
    chrono::DateTime::from_timestamp(secs, sub)
}

/// One span in, zero or more signals out.
fn span_signals(
    ctx: &mut LineCtx,
    name: Option<&str>,
    attributes: &[Attribute],
    span_id: Option<&str>,
    trace_id: Option<&str>,
    at: Option<chrono::DateTime<chrono::Utc>>,
    signals: &mut Vec<Signal>,
) {
    // `invoke_agent` repeats the session's totals using the same attribute
    // names. Counting it as well as the calls it summarises would double every
    // session exactly, and nothing in the result would look wrong.
    if name == Some(SPAN_SESSION) {
        return;
    }
    if name != Some(SPAN_CALL) {
        return;
    }

    let u = collect(attributes);
    if u.input.is_none() && u.output.is_none() {
        return; // a call span that carries no measurement
    }

    let session_id = u
        .session
        .clone()
        .or_else(|| trace_id.map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned());

    if ctx.current_session.as_deref() != Some(session_id.as_str()) {
        ctx.current_session = Some(session_id.clone());
        signals.push(Signal::SessionOpened {
            session_id: session_id.clone(),
            cwd: None,
            app_version: None,
        });
    }

    if let Some(model) = u.model.clone()
        && ctx.current_model.as_deref() != Some(model.as_str())
    {
        ctx.current_model = Some(model.clone());
        signals.push(Signal::ModelDeclared {
            session_id: session_id.clone(),
            model_id: model,
        });
    }

    let input = u.input.unwrap_or(0);
    let cache_read = u.cache_read.unwrap_or(0);

    // `gen_ai.usage.input_tokens` is the whole prompt, of which the cache read
    // is a part — the OpenAI convention rather than the Anthropic one. If the
    // part ever exceeds the whole, the reading is wrong and the honest move is
    // to record that rather than to wrap the subtraction into a vast number.
    let Some(input_fresh) = input.checked_sub(cache_read) else {
        signals.push(Signal::Anomaly {
            kind: "copilot_cache_exceeds_input".to_owned(),
            detail: format!(
                "a chat span reported {cache_read} cached of {input} input tokens; \
                 the span was not counted"
            ),
        });
        return;
    };

    let output = u.output.unwrap_or(0);
    if u.reasoning.is_some_and(|r| r > output) {
        signals.push(Signal::Anomaly {
            kind: "copilot_reasoning_exceeds_output".to_owned(),
            detail: format!(
                "a chat span reported {} reasoning of {output} output tokens; \
                 the span was not counted",
                u.reasoning.unwrap_or_default()
            ),
        });
        return;
    }

    ctx.event_ordinal = ctx.event_ordinal.saturating_add(1);

    let usage = TokenUsage::from_bands(TokenBands {
        input_fresh,
        cache_read,
        cache_write_5m: 0,
        cache_write_1h: 0,
        // Copilot reports no cache TTL, and assuming the cheaper tier would
        // understate a long session.
        cache_write_unspecified: u.cache_write.unwrap_or(0),
        output_total: output,
        reasoning: u.reasoning,
        unclassified: 0,
    });

    // Only the measurements, never the span: a chat span can carry the prompt
    // and the completion, and this string is stored.
    let raw = serde_json::json!({
        "gen_ai.usage.input_tokens": u.input,
        "gen_ai.usage.output_tokens": u.output,
        "gen_ai.usage.cache_read.input_tokens": u.cache_read,
        "gen_ai.usage.cache_creation.input_tokens": u.cache_write,
        "gen_ai.usage.reasoning.output_tokens": u.reasoning,
        "gen_ai.response.model": u.model,
    });

    signals.push(Signal::Usage(Box::new(UsageSignal {
        session_id,
        // The span id is unique per call and stable across re-reads, which is
        // exactly what makes re-ingesting a file free of duplicates.
        dedup_key: span_id.map_or_else(
            || format!("{ADAPTER_ID}:{}", ctx.event_ordinal),
            str::to_owned,
        ),
        model_id: u.model,
        occurred_at: at.unwrap_or_else(chrono::Utc::now),
        usage,
        measurement_source: SOURCE_OTEL_SPAN,
        request_kind: "assistant",
        is_sidechain: false,
        agent_id: None,
        agent_type: None,
        raw_json: Some(raw.to_string()),
    })));
}

pub fn parse_line(ctx: &mut LineCtx, line: &[u8]) -> ParseOutcome {
    let envelope: Envelope = match serde_json::from_slice(line) {
        Ok(e) => e,
        Err(e) => {
            return ParseOutcome::Malformed {
                reason: format!("not valid JSON: {e}"),
            };
        }
    };

    let mut signals = Vec::new();

    for resource in &envelope.resource_spans {
        for scope in &resource.scope_spans {
            for span in &scope.spans {
                span_signals(
                    ctx,
                    span.name.as_deref(),
                    &span.attributes,
                    span.span_id.as_deref(),
                    span.trace_id.as_deref(),
                    nanos_to_utc(span.end_time.as_ref().or(span.start_time.as_ref())),
                    &mut signals,
                );
            }
        }
    }

    // A bare span, for exporters that skip the envelope.
    if envelope.resource_spans.is_empty() {
        span_signals(
            ctx,
            envelope.name.as_deref(),
            &envelope.attributes,
            envelope.span_id.as_deref(),
            envelope.trace_id.as_deref(),
            nanos_to_utc(envelope.end_time.as_ref().or(envelope.start_time.as_ref())),
            &mut signals,
        );
    }

    if signals.is_empty() {
        ParseOutcome::Ignored
    } else {
        ParseOutcome::Signals(signals)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// One OTLP/JSON line holding the spans given, as the file exporter writes it.
    fn envelope(spans: &str) -> Vec<u8> {
        format!(r#"{{"resourceSpans":[{{"scopeSpans":[{{"spans":[{spans}]}}]}}]}}"#).into_bytes()
    }

    fn attr_int(key: &str, v: u64) -> String {
        format!(r#"{{"key":"{key}","value":{{"intValue":"{v}"}}}}"#)
    }

    fn attr_str(key: &str, v: &str) -> String {
        format!(r#"{{"key":"{key}","value":{{"stringValue":"{v}"}}}}"#)
    }

    fn chat_span(input: u64, output: u64, extra: &[String]) -> String {
        let mut attrs = vec![
            attr_int(ATTR_INPUT, input),
            attr_int(ATTR_OUTPUT, output),
            attr_str(ATTR_SESSION, "conv-1"),
            attr_str(ATTR_MODEL_RESPONSE, "gpt-5.5"),
        ];
        attrs.extend_from_slice(extra);
        format!(
            r#"{{"name":"chat","spanId":"aa01","traceId":"tt01",
               "endTimeUnixNano":"1786000000000000000","attributes":[{}]}}"#,
            attrs.join(",")
        )
    }

    fn parse(line: &[u8]) -> Vec<Signal> {
        let mut ctx = LineCtx::default();
        match parse_line(&mut ctx, line) {
            ParseOutcome::Signals(s) => s,
            other => panic!("expected signals, got {other:?}"),
        }
    }

    fn usage_of(signals: &[Signal]) -> Vec<&UsageSignal> {
        signals
            .iter()
            .filter_map(|s| match s {
                Signal::Usage(u) => Some(u.as_ref()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_chat_span_becomes_one_measured_request() {
        let signals = parse(&envelope(&chat_span(1000, 200, &[])));
        let usage = usage_of(&signals);
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].usage.input_fresh(), 1000);
        assert_eq!(usage[0].usage.output_total(), 200);
        assert_eq!(usage[0].model_id.as_deref(), Some("gpt-5.5"));
        assert_eq!(usage[0].session_id, "conv-1");
        assert_eq!(usage[0].dedup_key, "aa01");
    }

    #[test]
    fn the_session_rollup_span_is_never_counted() {
        // The trap this adapter is shaped around. `invoke_agent` carries the
        // session's totals under the *same* attribute names as the calls it
        // summarises, so counting both doubles every session exactly — and a
        // doubled total looks entirely plausible.
        let rollup = format!(
            r#"{{"name":"invoke_agent","spanId":"bb01","attributes":[{},{}]}}"#,
            attr_int(ATTR_INPUT, 1000),
            attr_int(ATTR_OUTPUT, 200)
        );
        let line = envelope(&format!("{},{rollup}", chat_span(1000, 200, &[])));

        let usage = usage_of(&parse(&line)).len();
        assert_eq!(usage, 1, "the rollup span must not be counted");

        // And on its own it produces nothing at all.
        let mut ctx = LineCtx::default();
        assert!(matches!(
            parse_line(&mut ctx, &envelope(&rollup)),
            ParseOutcome::Ignored
        ));
    }

    #[test]
    fn a_cache_read_is_taken_out_of_the_input_it_is_part_of() {
        // Copilot follows the OpenAI convention: input_tokens is the whole
        // prompt and the cache read is a part of it. Adding them would charge
        // the cached portion twice, at the full rate the second time.
        let signals = parse(&envelope(&chat_span(
            1000,
            200,
            &[attr_int(ATTR_CACHE_READ, 900)],
        )));
        let u = usage_of(&signals);
        assert_eq!(u[0].usage.input_fresh(), 100);
        assert_eq!(u[0].usage.cache_read(), 900);
        assert_eq!(u[0].usage.input_side_total(), 1000);
    }

    #[test]
    fn a_cache_read_larger_than_the_input_is_refused_rather_than_wrapped() {
        // u64 subtraction here would produce ~1.8e19 tokens and a cost to
        // match. The span is dropped and the contradiction recorded.
        let signals = parse(&envelope(&chat_span(
            100,
            10,
            &[attr_int(ATTR_CACHE_READ, 900)],
        )));
        assert!(usage_of(&signals).is_empty());
        assert!(signals.iter().any(|s| matches!(
            s,
            Signal::Anomaly { kind, .. } if kind == "copilot_cache_exceeds_input"
        )));
    }

    #[test]
    fn reasoning_that_is_absent_stays_unreported_rather_than_becoming_zero() {
        // Copilot emits the reasoning attribute "when available". A missing
        // attribute rendered as 0 would claim the model did no reasoning.
        let without = parse(&envelope(&chat_span(10, 10, &[])));
        assert_eq!(usage_of(&without)[0].usage.reasoning(), None);

        let with = parse(&envelope(&chat_span(
            10,
            10,
            &[attr_int(ATTR_REASONING, 4)],
        )));
        assert_eq!(usage_of(&with)[0].usage.reasoning(), Some(4));
    }

    #[test]
    fn reasoning_larger_than_output_is_refused() {
        let signals = parse(&envelope(&chat_span(10, 5, &[attr_int(ATTR_REASONING, 9)])));
        assert!(usage_of(&signals).is_empty());
        assert!(signals.iter().any(|s| matches!(
            s,
            Signal::Anomaly { kind, .. } if kind == "copilot_reasoning_exceeds_output"
        )));
    }

    #[test]
    fn counts_are_read_whether_they_arrive_as_strings_or_numbers() {
        // proto3 JSON mandates strings for 64-bit integers; real exporters
        // emit both. Reading one shape only would silently lose every count
        // from an exporter that chose the other.
        let numeric = r#"{"name":"chat","spanId":"cc01","attributes":[
            {"key":"gen_ai.usage.input_tokens","value":{"intValue":42}},
            {"key":"gen_ai.usage.output_tokens","value":{"intValue":7}}]}"#;
        let u = parse(&envelope(numeric));
        let u = usage_of(&u);
        assert_eq!(u[0].usage.input_fresh(), 42);
        assert_eq!(u[0].usage.output_total(), 7);
    }

    #[test]
    fn the_answering_model_wins_over_the_requested_one() {
        // They differ when the service routes elsewhere, and what answered is
        // what was billed.
        let span = format!(
            r#"{{"name":"chat","spanId":"dd01","attributes":[{},{},{},{}]}}"#,
            attr_int(ATTR_INPUT, 10),
            attr_int(ATTR_OUTPUT, 10),
            attr_str(ATTR_MODEL_REQUEST, "asked-for"),
            attr_str(ATTR_MODEL_RESPONSE, "answered-by")
        );
        let signals = parse(&envelope(&span));
        assert_eq!(
            usage_of(&signals)[0].model_id.as_deref(),
            Some("answered-by")
        );
    }

    #[test]
    fn a_span_with_no_measurement_is_ignored_rather_than_recorded_as_empty() {
        let span = r#"{"name":"chat","spanId":"ee01","attributes":[
            {"key":"gen_ai.conversation.id","value":{"stringValue":"c"}}]}"#;
        let mut ctx = LineCtx::default();
        assert!(matches!(
            parse_line(&mut ctx, &envelope(span)),
            ParseOutcome::Ignored
        ));
    }

    #[test]
    fn the_audit_trail_holds_the_counts_and_not_the_conversation() {
        // A chat span can carry the prompt and the completion. What is stored
        // is built from a fixed list of measurement attributes, so content
        // cannot reach the database by being added to the span later.
        let span = format!(
            r#"{{"name":"chat","spanId":"ff01","attributes":[{},{},{}]}}"#,
            attr_int(ATTR_INPUT, 10),
            attr_int(ATTR_OUTPUT, 10),
            attr_str("gen_ai.prompt", "a secret the user typed")
        );
        let signals = parse(&envelope(&span));
        let raw = usage_of(&signals)[0].raw_json.clone().unwrap();
        assert!(!raw.contains("secret"), "{raw}");
        assert!(!raw.contains("gen_ai.prompt"), "{raw}");
        assert!(raw.contains("input_tokens"));
    }

    #[test]
    fn a_session_and_its_model_are_announced_once() {
        let mut ctx = LineCtx::default();
        let first = parse_line(&mut ctx, &envelope(&chat_span(10, 10, &[])));
        let second = parse_line(&mut ctx, &envelope(&chat_span(20, 20, &[])));

        let count = |o: &ParseOutcome, f: fn(&Signal) -> bool| match o {
            ParseOutcome::Signals(s) => s.iter().filter(|x| f(x)).count(),
            _ => 0,
        };
        assert_eq!(
            count(&first, |s| matches!(s, Signal::SessionOpened { .. })),
            1
        );
        assert_eq!(
            count(&second, |s| matches!(s, Signal::SessionOpened { .. })),
            0
        );
        assert_eq!(
            count(&second, |s| matches!(s, Signal::ModelDeclared { .. })),
            0
        );
    }

    #[test]
    fn the_span_id_is_the_identity_so_re_reading_a_file_is_free() {
        let mut a = LineCtx::default();
        let mut b = LineCtx::default();
        let line = envelope(&chat_span(10, 10, &[]));
        let first = parse_line(&mut a, &line);
        let again = parse_line(&mut b, &line);
        let key = |o: &ParseOutcome| match o {
            ParseOutcome::Signals(s) => s
                .iter()
                .find_map(|x| match x {
                    Signal::Usage(u) => Some(u.dedup_key.clone()),
                    _ => None,
                })
                .unwrap(),
            _ => panic!("no usage"),
        };
        assert_eq!(key(&first), key(&again));
    }

    #[test]
    fn a_line_that_is_not_json_is_reported_rather_than_dropped() {
        let mut ctx = LineCtx::default();
        assert!(matches!(
            parse_line(&mut ctx, b"{not json"),
            ParseOutcome::Malformed { .. }
        ));
    }

    #[test]
    fn the_prefilter_admits_a_real_export_line() {
        use crate::UsageAdapter as _;
        let adapter = super::super::CopilotAdapter;
        let line = envelope(&chat_span(1, 1, &[]));
        assert!(adapter.is_candidate_line(&line));
        assert!(!adapter.is_candidate_line(b"{\"type\":\"assistant\"}"));
    }
}
