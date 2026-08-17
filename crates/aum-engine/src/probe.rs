//! Working out what each application can actually tell us.
//!
//! The capability matrix must reflect reality, so it is not written down
//! anywhere. It is derived by running the *same parser the ingest path uses*
//! over a sample of the application's own recent files and recording which
//! normalized fields real data populated. Each claim carries the evidence that
//! produced it — an actual value from an actual line.
//!
//! That matters because these tools change under us. A hardcoded table would
//! keep asserting that Codex reports reasoning tokens long after it stopped,
//! and nothing would notice. Deriving it from evidence means the matrix degrades
//! honestly instead: an absent field becomes `Unsupported` with a reason, and a
//! field we have not yet seen becomes `Unknown`, which renders as `?` and never
//! as yes.
//!
//! Claude Desktop is included precisely because it can tell us nothing. Showing
//! it as "detected, token telemetry unavailable, and here is why" is far more
//! useful than leaving it out and letting someone wonder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aum_adapters::{LineCtx, ParseOutcome, Signal, UsageAdapter};
use aum_contract::{AdapterDescriptor, AdapterState, CapabilityState};

/// How much of each sampled file to read.
///
/// The tail, not the head: a session's later lines are the ones that show what
/// the current version emits.
const SAMPLE_BYTES: u64 = 512 * 1024;
/// How many recent files to sample per adapter.
const SAMPLE_FILES: usize = 5;

/// The things an application might be able to tell us.
pub const CAPABILITIES: &[&str] = &[
    "exact_token_counts",
    "cache_read_tokens",
    "cache_write_tokens",
    "cache_ttl_breakdown",
    "reasoning_tokens",
    "model_identity",
    "per_request_latency",
    "provider_reported_cost",
    "actual_billed_cost",
    "sub_agent_attribution",
    "failure_visibility",
];

fn supported(evidence: impl Into<String>) -> CapabilityState {
    CapabilityState::Supported {
        evidence: evidence.into(),
    }
}

fn unsupported(reason: impl Into<String>) -> CapabilityState {
    CapabilityState::Unsupported {
        reason: reason.into(),
    }
}

fn unknown(reason: impl Into<String>) -> CapabilityState {
    CapabilityState::Unknown {
        reason: reason.into(),
    }
}

/// What a sample of real data showed.
#[derive(Debug, Default)]
struct Observed {
    lines: u64,
    usage_rows: u64,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    cache_write_1h: Option<u64>,
    reasoning: Option<u64>,
    /// Rows that reported no reasoning at all, which is a positive observation
    /// rather than an absence of one.
    reasoning_absent: u64,
    model: Option<String>,
    sidechain: bool,
    failures: u64,
    unclassified: Option<u64>,
}

/// Probe one adapter against its own recent files.
pub fn probe(
    adapter: &dyn UsageAdapter,
    root: &Path,
    matches: &dyn Fn(&Path) -> bool,
) -> Observed2 {
    let mut files = Vec::new();
    collect(root, matches, &mut files);
    files.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    files.reverse();
    files.truncate(SAMPLE_FILES);

    let mut observed = Observed::default();
    let sampled = files.len();

    for path in &files {
        let Ok(bytes) = read_tail(path, SAMPLE_BYTES) else {
            continue;
        };
        // Seeded with a placeholder session on purpose. Reading a file's tail
        // means the `session_meta` line that opens it is far behind us, and the
        // Codex parser rightly refuses to attribute usage it cannot place — so
        // without this the probe would conclude that Codex reports nothing at
        // all. The probe is asking which *fields* the application populates,
        // not who the usage belongs to, and nothing it produces is recorded.
        let mut ctx = LineCtx {
            current_session: Some("probe".to_owned()),
            ..LineCtx::default()
        };

        // The first line of a tail is usually a fragment; skipping it avoids a
        // spurious malformed reading.
        for line in bytes.split(|b| *b == b'\n').skip(1) {
            if line.is_empty() || !adapter.is_candidate_line(line) {
                continue;
            }
            observed.lines = observed.lines.saturating_add(1);

            let ParseOutcome::Signals(signals) = adapter.parse_line(&mut ctx, line) else {
                continue;
            };
            for signal in signals {
                match signal {
                    Signal::Usage(u) => {
                        observed.usage_rows = observed.usage_rows.saturating_add(1);
                        let usage = &u.usage;
                        note(&mut observed.cache_read, usage.cache_read());
                        note(&mut observed.cache_write, usage.cache_write_total());
                        note(&mut observed.cache_write_1h, usage.cache_write_1h());
                        note(&mut observed.unclassified, usage.unclassified());
                        match usage.reasoning() {
                            Some(r) => note(&mut observed.reasoning, r),
                            None => {
                                observed.reasoning_absent =
                                    observed.reasoning_absent.saturating_add(1);
                            }
                        }
                        if u.is_sidechain {
                            observed.sidechain = true;
                        }
                        if observed.model.is_none() {
                            observed.model.clone_from(&u.model_id);
                        }
                    }
                    Signal::ModelDeclared { model_id, .. } if observed.model.is_none() => {
                        observed.model = Some(model_id);
                    }
                    Signal::RequestFailed { .. } => {
                        observed.failures = observed.failures.saturating_add(1);
                    }
                    _ => {}
                }
            }
        }
    }

    Observed2 {
        observed,
        files_sampled: sampled,
    }
}

/// A probe result, with how much was looked at.
pub struct Observed2 {
    observed: Observed,
    files_sampled: usize,
}

/// Record the largest non-zero value seen for a field.
///
/// Zero is not evidence: a request that happened to read nothing from cache
/// tells us nothing about whether the field exists.
fn note(slot: &mut Option<u64>, value: u64) {
    if value > 0 && slot.is_none_or(|current| value > current) {
        *slot = Some(value);
    }
}

fn collect(dir: &Path, matches: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, matches, out);
        } else if matches(&path) {
            out.push(path);
        }
    }
}

fn read_tail(path: &Path, bytes: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let start = size.saturating_sub(bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(usize::try_from(size.min(bytes)).unwrap_or(0));
    file.take(bytes).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Turn observations into a capability matrix.
#[must_use]
pub fn describe_file_adapter(
    adapter_id: &str,
    display_name: &str,
    result: &Observed2,
    executable: Option<String>,
    installed: bool,
) -> AdapterDescriptor {
    let o = &result.observed;
    let mut capabilities: BTreeMap<&str, CapabilityState> = BTreeMap::new();
    let mut notes = Vec::new();

    if !installed {
        for name in CAPABILITIES {
            capabilities.insert(
                name,
                unknown("This application was not found on this machine."),
            );
        }
        notes.push(format!(
            "{display_name} was not found. Nothing can be measured until it has run at least once."
        ));
        return finish(
            adapter_id,
            display_name,
            AdapterState::NotInstalled,
            executable,
            capabilities,
            notes,
        );
    }

    if o.usage_rows == 0 {
        // Installed, but nothing observed. Genuinely unknown rather than
        // unsupported: absence of evidence here is not evidence of absence.
        for name in CAPABILITIES {
            capabilities.insert(
                name,
                unknown("No usage has been observed yet, so this could not be checked."),
            );
        }
        notes.push(format!(
            "{display_name} is installed but has produced no usage the monitor can read yet. \
             Run it once and this will fill in."
        ));
        return finish(
            adapter_id,
            display_name,
            AdapterState::Detected,
            executable,
            capabilities,
            notes,
        );
    }

    capabilities.insert(
        "exact_token_counts",
        supported(format!(
            "{} usage records read from {} recent file(s); the counts are the provider's own, \
             relayed verbatim",
            o.usage_rows, result.files_sampled
        )),
    );

    capabilities.insert(
        "cache_read_tokens",
        match o.cache_read {
            Some(v) => supported(format!("observed cache_read = {v}")),
            None => unknown("No cached read was seen in the sample."),
        },
    );

    capabilities.insert(
        "cache_write_tokens",
        match o.cache_write {
            Some(v) => supported(format!("observed cache write = {v}")),
            None => unknown("No cache write was seen in the sample."),
        },
    );

    capabilities.insert(
        "cache_ttl_breakdown",
        match o.cache_write_1h {
            Some(v) => supported(format!(
                "observed a 1-hour cache write of {v}, so writes can be priced by TTL"
            )),
            None if o.cache_write.is_some() => unsupported(
                "Cache writes are reported without a time-to-live breakdown, so they cannot be \
                 priced by tier and are counted at the cheaper rate.",
            ),
            None => unknown("No cache write was seen in the sample."),
        },
    );

    capabilities.insert(
        "reasoning_tokens",
        match (o.reasoning, o.reasoning_absent) {
            (Some(v), _) => supported(format!("observed reasoning_tokens = {v}")),
            // Every row said nothing, which is a positive finding.
            (None, absent) if absent > 0 => unsupported(format!(
                "{absent} usage records were read and none carried a reasoning-token count; this \
                 application does not report one"
            )),
            _ => unknown("Nothing was observed."),
        },
    );

    capabilities.insert(
        "model_identity",
        match &o.model {
            Some(m) => supported(format!("observed model {m}")),
            None => {
                unsupported("No model identifier accompanies the usage this application writes.")
            }
        },
    );

    capabilities.insert(
        "per_request_latency",
        unsupported(
            "The files this application writes contain no latency or time-to-first-token field. \
             The interval between messages is not a substitute: it contains tool execution, retry \
             backoff and think time.",
        ),
    );

    capabilities.insert(
        "provider_reported_cost",
        unsupported("No cost figure appears in the files this application writes."),
    );

    capabilities.insert(
        "actual_billed_cost",
        unsupported(
            "Usage here is billed by subscription, not per token, so there is no per-task charge \
             to report.",
        ),
    );

    capabilities.insert(
        "sub_agent_attribution",
        if o.sidechain {
            supported("sub-agent records were seen carrying the parent session id")
        } else {
            unknown("No sub-agent activity was seen in the sample.")
        },
    );

    // `launch_with_pinned_session` was here while the monitor could start
    // agents itself. It reads history now, so the row would have described a
    // capability of a feature that no longer exists — the most convincing kind
    // of wrong answer, since everything about it was once true.

    capabilities.insert(
        "failure_visibility",
        if o.failures > 0 {
            supported(format!(
                "{} failed request(s) observed in the sample",
                o.failures
            ))
        } else {
            unknown("No failure was seen in the sample, which is the usual case.")
        },
    );

    if o.unclassified.is_some() {
        notes.push(
            "Some records report a token total without saying whether it was input or output. \
             Those tokens are counted but cannot be priced."
                .to_owned(),
        );
    }

    finish(
        adapter_id,
        display_name,
        AdapterState::Ready,
        executable,
        capabilities,
        notes,
    )
}

/// Claude Desktop, which reports one number and no structure.
///
/// Included deliberately, and described precisely, because "this application
/// tells you nothing" turned out to be too strong. It writes two quantitative
/// files: a plan-limit percentage sampler, from which no token count follows by
/// any defensible means, and a single running daily token count. The count is
/// real. It also has no model, no input/output split, no cache breakdown and no
/// request boundary, so every capability below is genuinely unsupported — each
/// one asks a question this number cannot answer — and the count is carried
/// separately, as itself.
#[must_use]
pub fn describe_claude_desktop(
    installed: bool,
    daily: Option<aum_contract::DailyTotal>,
) -> AdapterDescriptor {
    let mut capabilities: BTreeMap<&str, CapabilityState> = BTreeMap::new();

    let has_count = daily.is_some();
    let general = if has_count {
        "Claude Desktop writes one running token total per day and plan-limit percentages. \
         Neither carries a model, an input/output split, or a conversation."
    } else {
        "Claude Desktop records only plan-limit percentages for five-hour and seven-day windows. \
         There is no token, model or per-conversation data to read."
    };

    for name in CAPABILITIES {
        capabilities.insert(name, unsupported(general));
    }

    // The one capability whose reason is genuinely different once a count
    // exists: the tokens are there, the per-request structure is not.
    if has_count {
        capabilities.insert(
            "exact_token_counts",
            unsupported(
                "A whole-application running total for the current day is available, but no \
                 per-request counts. It cannot be attributed to a task, split into input and \
                 output, or priced.",
            ),
        );
    }

    let mut notes = vec![
        if has_count {
            "Detected. It reports a single daily token total and nothing more granular."
        } else {
            "Detected, but no token telemetry exists to collect."
        }
        .to_owned(),
        "Its plan-limit sampler records percentages of a five-hour and a seven-day window, which \
         cannot be converted into a token count by any defensible means."
            .to_owned(),
    ];
    if has_count {
        notes.push(
            "The daily total is kept here because Claude Desktop keeps only the current day and \
             discards it at midnight."
                .to_owned(),
        );
    }
    notes.push(
        "To measure a Claude session per request — by model, split into input and output, and \
         costed — use Claude Code, or route an API client through the local proxy."
            .to_owned(),
    );

    let mut descriptor = finish(
        "claude_desktop",
        "Claude Desktop",
        if installed {
            AdapterState::Detected
        } else {
            AdapterState::NotInstalled
        },
        None,
        capabilities,
        notes,
    );
    descriptor.daily_total = daily;
    descriptor
}

/// Assemble the daily total from what has been sampled.
///
/// The scope sentence travels with the value rather than sitting in a tooltip,
/// so that a screenshot of the number cannot claim more than the number
/// supports.
#[must_use]
pub fn daily_total_from(rows: &[aum_db::desktop::DailyRow]) -> Option<aum_contract::DailyTotal> {
    let latest = rows.first()?;
    Some(aum_contract::DailyTotal {
        day: latest.day.clone(),
        // Application telemetry, not a provider-reported figure: Claude Desktop
        // computed this itself, and nothing in the file shows the provider's own
        // per-request usage. Classified by who authored the number.
        tokens: aum_contract::Measured::calculated(
            u64::try_from(latest.tokens).unwrap_or(0),
            aum_contract::MeasurementSource::ApplicationTelemetry,
        ),
        scope: "Every token Claude Desktop counted for the whole application on this day. It \
                carries no model, no input/output split and no conversation, so it cannot be \
                attributed to a task or priced."
            .to_owned(),
        history: rows
            .iter()
            .map(|r| aum_contract::DailyPoint {
                day: r.day.clone(),
                tokens: u64::try_from(r.tokens).unwrap_or(0),
            })
            .collect(),
    })
}

fn finish(
    id: &str,
    display_name: &str,
    state: AdapterState,
    executable_path: Option<String>,
    capabilities: BTreeMap<&str, CapabilityState>,
    notes: Vec<String>,
) -> AdapterDescriptor {
    AdapterDescriptor {
        id: id.to_owned(),
        display_name: display_name.to_owned(),
        state,
        app_version: None,
        adapter_version: env!("CARGO_PKG_VERSION").to_owned(),
        executable_path,
        capabilities: capabilities
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
        notes,
        daily_total: None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use aum_adapters::claude_code::ClaudeCodeAdapter;
    use aum_adapters::codex::CodexAdapter;

    fn jsonl(p: &Path) -> bool {
        p.extension().is_some_and(|e| e == "jsonl")
    }

    fn state<'a>(d: &'a AdapterDescriptor, name: &str) -> &'a CapabilityState {
        &d.capabilities
            .iter()
            .find(|(k, _)| k == name)
            .unwrap_or_else(|| panic!("no capability {name}"))
            .1
    }

    fn claude_line(output: u64, cache_1h: u64) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"s1","timestamp":"2026-08-12T19:10:15.774Z",
              "requestId":"req_{output}","message":{{"id":"msg_{output}","model":"claude-opus-5",
              "usage":{{"input_tokens":2,"output_tokens":{output},
                "cache_creation_input_tokens":{cache_1h},"cache_read_input_tokens":30885,
                "cache_creation":{{"ephemeral_5m_input_tokens":0,
                  "ephemeral_1h_input_tokens":{cache_1h}}}}}}}}}"#
        )
        .replace('\n', "")
    }

    #[test]
    fn an_uninstalled_application_claims_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let result = probe(&ClaudeCodeAdapter, dir.path(), &jsonl);
        let d = describe_file_adapter("claude_code", "Claude Code", &result, None, false);

        assert_eq!(d.state, AdapterState::NotInstalled);
        for (_, capability) in &d.capabilities {
            assert!(
                matches!(capability, CapabilityState::Unknown { .. }),
                "an absent application must claim nothing, not deny everything"
            );
        }
    }

    #[test]
    fn installed_but_unused_is_unknown_rather_than_unsupported() {
        // Absence of evidence is not evidence of absence.
        let dir = tempfile::tempdir().unwrap();
        let result = probe(&ClaudeCodeAdapter, dir.path(), &jsonl);
        let d = describe_file_adapter("claude_code", "Claude Code", &result, None, true);

        assert_eq!(d.state, AdapterState::Detected);
        assert!(matches!(
            state(&d, "exact_token_counts"),
            CapabilityState::Unknown { .. }
        ));
    }

    #[test]
    fn claude_code_capabilities_come_from_the_data_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let lines: Vec<String> = (1..=4).map(|i| claude_line(i * 100, 23_610)).collect();
        // A leading line, because the tail reader skips its first fragment.
        std::fs::write(&path, format!("{{}}\n{}\n", lines.join("\n"))).unwrap();

        let result = probe(&ClaudeCodeAdapter, dir.path(), &jsonl);
        let d = describe_file_adapter("claude_code", "Claude Code", &result, None, true);

        assert_eq!(d.state, AdapterState::Ready);
        assert!(matches!(
            state(&d, "exact_token_counts"),
            CapabilityState::Supported { .. }
        ));

        // The TTL breakdown is present, and the evidence quotes the real value.
        let CapabilityState::Supported { evidence } = state(&d, "cache_ttl_breakdown") else {
            panic!("expected the TTL breakdown to be supported")
        };
        assert!(
            evidence.contains("23610"),
            "evidence should quote it: {evidence}"
        );
    }

    #[test]
    fn claude_code_reasoning_is_unsupported_on_the_evidence_of_its_absence() {
        // The distinction that matters: not "we did not look", but "we read
        // four records and none carried one".
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("s.jsonl"),
            format!(
                "{{}}\n{}\n",
                (1..=4)
                    .map(|i| claude_line(i * 100, 0))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .unwrap();

        let result = probe(&ClaudeCodeAdapter, dir.path(), &jsonl);
        let d = describe_file_adapter("claude_code", "Claude Code", &result, None, true);

        let CapabilityState::Unsupported { reason } = state(&d, "reasoning_tokens") else {
            panic!(
                "expected reasoning to be unsupported, got {:?}",
                state(&d, "reasoning_tokens")
            )
        };
        assert!(reason.contains("none carried"), "{reason}");
    }

    #[test]
    fn codex_reasoning_is_supported_with_the_value_that_proved_it() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            r#"{"type":"session_meta","payload":{"id":"s1","cwd":"/x"}}"#.to_owned(),
            r#"{"type":"turn_context","payload":{"model":"gpt-5.5"}}"#.to_owned(),
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{
               "total_token_usage":{"input_tokens":18517,"cached_input_tokens":12160,
                 "output_tokens":328,"reasoning_output_tokens":106,"total_tokens":18845},
               "last_token_usage":{"input_tokens":18517,"cached_input_tokens":12160,
                 "output_tokens":328,"reasoning_output_tokens":106,"total_tokens":18845}}}}"#
                .replace('\n', ""),
        ];
        std::fs::write(
            dir.path().join("rollout-x.jsonl"),
            format!("{{}}\n{}\n", lines.join("\n")),
        )
        .unwrap();

        let result = probe(&CodexAdapter, dir.path(), &jsonl);
        let d = describe_file_adapter("codex", "Codex", &result, None, true);

        let CapabilityState::Supported { evidence } = state(&d, "reasoning_tokens") else {
            panic!("Codex does report reasoning tokens")
        };
        assert!(evidence.contains("106"), "{evidence}");
    }

    #[test]
    fn latency_is_unsupported_and_explains_why_the_obvious_substitute_is_not_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("s.jsonl"),
            format!("{{}}\n{}\n", claude_line(100, 0)),
        )
        .unwrap();

        let result = probe(&ClaudeCodeAdapter, dir.path(), &jsonl);
        let d = describe_file_adapter("claude_code", "Claude Code", &result, None, true);

        let CapabilityState::Unsupported { reason } = state(&d, "per_request_latency") else {
            panic!("transcripts carry no latency")
        };
        assert!(reason.contains("retry backoff"), "{reason}");
    }

    #[test]
    fn no_capability_describes_a_feature_the_monitor_no_longer_has() {
        // The monitor used to launch agents, and reported per-adapter how well
        // it could pin a session id. A row that outlives its feature is worse
        // than no row: every word of it is plausible.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rollout-x.jsonl"),
            "{}\n".to_owned()
                + r#"{"type":"session_meta","payload":{"id":"s1"}}"#
                + "\n"
                + r#"{"type":"event_msg","payload":{"type":"token_count","info":{
                   "total_token_usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15},
                   "last_token_usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}}"#
                    .replace('\n', "")
                    .as_str()
                + "\n",
        )
        .unwrap();

        let result = probe(&CodexAdapter, dir.path(), &jsonl);
        let d = describe_file_adapter("codex", "Codex", &result, None, true);

        let reported: Vec<&str> = d.capabilities.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            !reported.iter().any(|k| k.contains("launch")),
            "the monitor no longer launches anything: {reported:?}"
        );
        assert!(
            reported.contains(&"exact_token_counts"),
            "the reading capabilities are still reported: {reported:?}"
        );
    }

    fn a_daily_total() -> aum_contract::DailyTotal {
        daily_total_from(&[aum_db::desktop::DailyRow {
            day: "2026-08-13".to_owned(),
            tokens: 1_119_656,
            last_seen_at: "2026-08-13T12:00:00.000Z".to_owned(),
        }])
        .unwrap()
    }

    #[test]
    fn claude_desktop_says_what_it_cannot_do_and_why() {
        let d = describe_claude_desktop(true, None);
        assert_eq!(d.state, AdapterState::Detected);

        for (name, capability) in &d.capabilities {
            assert!(
                matches!(capability, CapabilityState::Unsupported { .. }),
                "{name} should be unsupported for Claude Desktop"
            );
        }
        let notes = d.notes.join(" ");
        assert!(
            notes.contains("plan-limit") && notes.contains("percentages"),
            "the reason must be stated, not just the verdict: {notes}"
        );
        assert!(
            d.notes.iter().any(|n| n.contains("Claude Code")),
            "and it should say what to use instead"
        );
    }

    #[test]
    fn a_daily_total_does_not_make_any_capability_supported() {
        // The count is real and every question the matrix asks — per request,
        // by model, split into input and output — is still unanswerable. If a
        // capability ever flipped to supported here, the number would start
        // being treated as something it is not.
        let d = describe_claude_desktop(true, Some(a_daily_total()));
        for (name, capability) in &d.capabilities {
            assert!(
                matches!(capability, CapabilityState::Unsupported { .. }),
                "{name} must stay unsupported even once a daily total exists"
            );
        }
    }

    #[test]
    fn the_daily_total_carries_its_own_limits() {
        let d = describe_claude_desktop(true, Some(a_daily_total()));
        let total = d.daily_total.expect("the total should be carried");
        assert_eq!(total.tokens.value, Some(1_119_656));
        // Calculated, not exact: Claude Desktop computed this, the provider did
        // not send it to us.
        assert_eq!(
            total.tokens.display_kind(),
            aum_contract::DisplayKind::Calculated
        );
        for phrase in ["whole application", "cannot be attributed", "priced"] {
            assert!(
                total.scope.contains(phrase),
                "the scope sentence should say {phrase:?}: {}",
                total.scope
            );
        }
    }

    #[test]
    fn without_a_reading_there_is_no_total_rather_than_a_zero() {
        let d = describe_claude_desktop(true, None);
        assert!(d.daily_total.is_none());
        assert!(daily_total_from(&[]).is_none());
    }

    #[test]
    fn every_capability_is_reported_for_every_adapter() {
        // A missing row would read as "not applicable" when it means
        // "we forgot to check".
        let d = describe_claude_desktop(true, None);
        for name in CAPABILITIES {
            assert!(
                d.capabilities.iter().any(|(k, _)| k == name),
                "{name} is missing from the matrix"
            );
        }
    }
}
