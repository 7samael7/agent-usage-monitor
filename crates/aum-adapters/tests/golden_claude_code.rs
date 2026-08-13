//! Golden-file test for the Claude Code parser.
//!
//! The fixture is a real 400-line session, put through
//! `scripts/redact-fixture.ts`: every number and structural field preserved
//! exactly, every piece of free text replaced. It contains genuine fan-out —
//! 233 lines carrying usage that describe only 132 distinct requests — which is
//! the pattern no hand-written fixture reproduces convincingly.
//!
//! The expected values below were computed from the fixture independently of
//! the Rust parser. If the parser drifts, these fail; if the fixture is
//! regenerated, they must be recomputed deliberately rather than adjusted until
//! the test passes.

// Integration tests: a failed setup should abort loudly, not be handled.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use aum_adapters::claude_code::ClaudeCodeAdapter;
use aum_adapters::{LineCtx, ParseOutcome, Signal, UsageAdapter};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/claude_code/session_with_fanout.jsonl")
}

#[derive(Default)]
struct Parsed {
    lines_with_usage: u64,
    naive_output: u64,
    naive_cache_write: u64,
    naive_cache_read: u64,
    naive_input_fresh: u64,
    deduped: HashMap<String, (u64, u64, u64, u64)>,
    failures: u64,
    retry_attempts: u64,
    compactions: u64,
    sidechain: HashSet<String>,
    malformed: u64,
    models: HashSet<String>,
}

fn parse_fixture() -> Parsed {
    let adapter = ClaudeCodeAdapter;
    let bytes = std::fs::read(fixture()).expect("fixture is missing; regenerate it");
    let mut ctx = LineCtx::default();
    let mut p = Parsed::default();

    for line in bytes.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if !adapter.is_candidate_line(line) {
            continue;
        }
        match adapter.parse_line(&mut ctx, line) {
            ParseOutcome::Malformed { .. } => p.malformed += 1,
            ParseOutcome::Ignored => {}
            ParseOutcome::Signals(signals) => {
                for signal in signals {
                    match signal {
                        Signal::Usage(u) => {
                            p.lines_with_usage += 1;
                            let usage = &u.usage;
                            p.naive_output += usage.output_total();
                            p.naive_cache_write += usage.cache_write_total();
                            p.naive_cache_read += usage.cache_read();
                            p.naive_input_fresh += usage.input_fresh();

                            let e = p.deduped.entry(u.dedup_key.clone()).or_default();
                            e.0 = e.0.max(usage.output_total());
                            e.1 = e.1.max(usage.cache_write_total());
                            e.2 = e.2.max(usage.cache_read());
                            e.3 = e.3.max(usage.input_fresh());

                            if u.is_sidechain {
                                p.sidechain.insert(u.dedup_key.clone());
                            }
                            if let Some(m) = &u.model_id {
                                p.models.insert(m.clone());
                            }
                        }
                        Signal::RequestFailed { .. } => p.failures += 1,
                        Signal::RetryAttempt { .. } => p.retry_attempts += 1,
                        Signal::ContextCompacted { .. } => p.compactions += 1,
                        _ => {}
                    }
                }
            }
        }
    }
    p
}

#[test]
fn the_parser_reads_every_line_of_a_real_session() {
    let p = parse_fixture();
    assert_eq!(
        p.malformed, 0,
        "the parser failed on real, current-format lines"
    );
    assert!(p.lines_with_usage > 0);
}

#[test]
fn a_contradictory_provider_row_is_kept_and_flagged_rather_than_dropped() {
    // Measured at 4 rows in this session and 4 in 14,449 across the corpus.
    // Dropping them loses 4,509 real cache-creation tokens; trusting the
    // summary field over the breakdown loses the same.
    let adapter = ClaudeCodeAdapter;
    let bytes = std::fs::read(fixture()).expect("fixture is missing");
    let mut ctx = LineCtx::default();
    let mut inconsistent = 0;

    for line in bytes.split(|b| *b == b'\n') {
        if line.is_empty() || !adapter.is_candidate_line(line) {
            continue;
        }
        if let ParseOutcome::Signals(signals) = adapter.parse_line(&mut ctx, line) {
            for signal in &signals {
                if let Signal::Anomaly { kind, .. } = signal
                    && kind == "provider_self_inconsistent"
                {
                    inconsistent += 1;
                    // The measurement is still delivered alongside the anomaly.
                    assert!(
                        signals.iter().any(|s| matches!(s, Signal::Usage(_))),
                        "the row was flagged but its tokens were thrown away"
                    );
                }
            }
        }
    }
    assert_eq!(
        inconsistent, 4,
        "expected exactly the four known contradictory rows"
    );
}

#[test]
fn deduplication_collapses_233_usage_lines_into_132_requests() {
    // The single highest-impact number in the ingest path.
    let p = parse_fixture();
    assert_eq!(p.lines_with_usage, 233, "usage-bearing lines");
    assert_eq!(p.deduped.len(), 132, "distinct requests");
}

/// Four of this session's usage objects report `cache_creation_input_tokens: 0`
/// while their TTL breakdown reports thousands of 1-hour tokens. Those 4,509
/// deduplicated tokens are real and are kept; an implementation that trusted the
/// summary field alone, or that rejected the contradictory rows outright, would
/// both undercount here.
#[test]
fn the_deduplicated_totals_are_the_correct_ones() {
    let p = parse_fixture();
    let mut output = 0;
    let mut cache_write = 0;
    let mut cache_read = 0;
    let mut input_fresh = 0;
    for (o, w, r, i) in p.deduped.values() {
        output += o;
        cache_write += w;
        cache_read += r;
        input_fresh += i;
    }

    assert_eq!(output, 95_716, "output tokens");
    assert_eq!(cache_write, 300_756, "cache-creation tokens");
    assert_eq!(cache_read, 17_934_762, "cache-read tokens");
    assert_eq!(input_fresh, 259, "fresh input tokens");
}

#[test]
fn summing_per_line_would_be_wrong_by_more_than_twice() {
    // Stated explicitly so the cost of getting this wrong stays visible in the
    // test output rather than only in a commit message.
    let p = parse_fixture();
    assert_eq!(p.naive_output, 206_166, "per-line output sum");
    assert_eq!(p.naive_cache_write, 707_377, "per-line cache-creation sum");
    assert_eq!(p.naive_cache_read, 31_904_231);
    assert_eq!(p.naive_input_fresh, 456);

    let deduped_output: u64 = p.deduped.values().map(|v| v.0).sum();
    let inflation = p.naive_output as f64 / deduped_output as f64;
    assert!(
        (2.15..2.16).contains(&inflation),
        "expected ~2.15x inflation from fan-out, got {inflation:.3}x"
    );
}

#[test]
fn the_compaction_boundary_contributes_no_tokens() {
    // preTokens on this boundary is large enough to dwarf the entire session.
    // It is a context-window size, not a request.
    let p = parse_fixture();
    assert_eq!(p.compactions, 1, "the fixture contains one compaction");

    let deduped_output: u64 = p.deduped.values().map(|v| v.0).sum();
    assert_eq!(
        deduped_output, 95_716,
        "unchanged by the compaction boundary"
    );
}

#[test]
fn reasoning_is_absent_for_every_request_in_the_session() {
    // Claude Code emits thinking blocks but no count for them. Every request
    // here must report reasoning as unknown, never as zero.
    let adapter = ClaudeCodeAdapter;
    let bytes = std::fs::read(fixture()).expect("fixture is missing");
    let mut ctx = LineCtx::default();
    let mut checked = 0;

    for line in bytes.split(|b| *b == b'\n') {
        if line.is_empty() || !adapter.is_candidate_line(line) {
            continue;
        }
        if let ParseOutcome::Signals(signals) = adapter.parse_line(&mut ctx, line) {
            for signal in signals {
                if let Signal::Usage(u) = signal {
                    assert_eq!(u.usage.reasoning(), None, "reasoning must stay unknown");
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 100, "expected to have checked the whole session");
}

#[test]
fn parsing_is_deterministic_and_repeatable() {
    // Idempotency at the parser level: the same bytes must always yield the
    // same keys and the same numbers, which is what lets ingest re-read a file
    // after a crash.
    let first = parse_fixture();
    let second = parse_fixture();
    assert_eq!(first.deduped.len(), second.deduped.len());
    assert_eq!(first.naive_output, second.naive_output);

    let mut a: Vec<_> = first.deduped.into_iter().collect();
    let mut b: Vec<_> = second.deduped.into_iter().collect();
    a.sort();
    b.sort();
    assert_eq!(a, b);
}

#[test]
fn the_fixture_contains_no_unredacted_content() {
    // The fixture is committed, so this is a privacy assertion, not a data one.
    // It fails loudly if someone regenerates the fixture without redacting, or
    // hand-copies a real transcript into place.
    let text = std::fs::read_to_string(fixture()).expect("fixture is missing");

    for forbidden in ["/Users/", "/home/", "sebastianhruby", "C:\\Users"] {
        assert!(
            !text.contains(forbidden),
            "fixture contains {forbidden}; it was not redacted"
        );
    }

    // Free text is replaced by runs of 'x'. A long alphabetic string that is not
    // filler suggests real prose survived.
    for line in text.lines() {
        for candidate in line.split('"') {
            if candidate.len() < 40 {
                continue;
            }
            let alphabetic = candidate.chars().filter(|c| c.is_alphabetic()).count();
            if alphabetic * 2 < candidate.len() {
                continue; // mostly punctuation or digits: ids, timestamps
            }
            let filler = candidate.chars().filter(|c| *c == 'x').count();
            assert!(
                filler * 10 > alphabetic * 9,
                "possible unredacted prose in fixture: {}",
                &candidate[..candidate.len().min(80)]
            );
        }
    }
}
