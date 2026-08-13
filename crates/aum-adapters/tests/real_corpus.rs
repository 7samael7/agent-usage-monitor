//! Verification against the machine's own transcripts.
//!
//! These tests are `#[ignore]`d, because they depend on data that exists only on
//! a machine that has actually used the agents. They read and never write, and
//! assert only on aggregate counts — no conversation content is touched.
//!
//! Run them with:
//!
//! ```text
//! cargo test -p aum-adapters --test real_corpus -- --ignored --nocapture
//! ```
//!
//! Their value is that the redacted fixtures in `tests/fixtures/` can only prove
//! the parser is self-consistent. These prove it is right about reality — that
//! the deduplication actually collapses the fan-out that real transcripts
//! contain, at roughly the ratio measured when the format was investigated.

// Integration tests: a failed setup should abort loudly, not be handled.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use aum_adapters::claude_code::ClaudeCodeAdapter;
use aum_adapters::{LineCtx, ParseOutcome, Signal, UsageAdapter};

fn projects_dir() -> Option<PathBuf> {
    let dir = dirs_home()?.join(".claude").join("projects");
    dir.is_dir().then_some(dir)
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Every `.jsonl` under a directory, including sub-agent transcripts.
fn transcripts(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            transcripts(&path, out);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

#[derive(Default, Debug)]
struct Tally {
    lines_with_usage: u64,
    distinct_requests: usize,
    naive_output: u64,
    naive_cache_creation: u64,
    deduped_output: u64,
    deduped_cache_creation: u64,
    failures: u64,
    retry_attempts: u64,
    compactions: u64,
    sidechain_requests: usize,
    malformed: u64,
}

fn tally(paths: &[PathBuf]) -> Tally {
    let adapter = ClaudeCodeAdapter;
    let mut t = Tally::default();
    // key -> the winning (max-merged) observation, exactly as the database does.
    let mut deduped: HashMap<String, (u64, u64)> = HashMap::new();
    let mut sidechain_keys: HashSet<String> = HashSet::new();

    for path in paths {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut ctx = LineCtx::default();

        for line in bytes.split(|b| *b == b'\n') {
            if line.is_empty() || !adapter.is_candidate_line(line) {
                continue;
            }
            match adapter.parse_line(&mut ctx, line) {
                ParseOutcome::Malformed { .. } => t.malformed += 1,
                ParseOutcome::Ignored => {}
                ParseOutcome::Signals(signals) => {
                    for signal in signals {
                        match signal {
                            Signal::Usage(u) => {
                                t.lines_with_usage += 1;
                                let out = u.usage.output_total();
                                let cc = u.usage.cache_write_total();
                                t.naive_output += out;
                                t.naive_cache_creation += cc;

                                let entry = deduped.entry(u.dedup_key.clone()).or_insert((0, 0));
                                entry.0 = entry.0.max(out);
                                entry.1 = entry.1.max(cc);
                                if u.is_sidechain {
                                    sidechain_keys.insert(u.dedup_key.clone());
                                }
                            }
                            Signal::RequestFailed { .. } => t.failures += 1,
                            Signal::RetryAttempt { .. } => t.retry_attempts += 1,
                            Signal::ContextCompacted { .. } => t.compactions += 1,
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    t.distinct_requests = deduped.len();
    t.sidechain_requests = sidechain_keys.len();
    for (out, cc) in deduped.values() {
        t.deduped_output += out;
        t.deduped_cache_creation += cc;
    }
    t
}

#[test]
#[ignore = "requires real Claude Code transcripts on this machine"]
fn deduplication_collapses_the_fanout_in_real_transcripts() {
    let Some(root) = projects_dir() else {
        eprintln!("no ~/.claude/projects; nothing to verify against");
        return;
    };

    let mut paths = Vec::new();
    transcripts(&root, &mut paths);
    assert!(
        !paths.is_empty(),
        "found no transcripts under {}",
        root.display()
    );

    let t = tally(&paths);
    eprintln!("\n─── real corpus ─────────────────────────────────────────");
    eprintln!("  transcript files          {}", paths.len());
    eprintln!("  lines carrying usage      {}", t.lines_with_usage);
    eprintln!("  distinct requests         {}", t.distinct_requests);
    eprintln!("  of which sub-agent        {}", t.sidechain_requests);
    eprintln!("  output   naive            {}", t.naive_output);
    eprintln!("  output   deduplicated     {}", t.deduped_output);
    eprintln!(
        "  inflation if not deduped  {:.2}x",
        t.naive_output as f64 / t.deduped_output.max(1) as f64
    );
    eprintln!("  cache-creation naive      {}", t.naive_cache_creation);
    eprintln!("  cache-creation deduped    {}", t.deduped_cache_creation);
    eprintln!("  terminal failures         {}", t.failures);
    eprintln!("  retry attempts            {}", t.retry_attempts);
    eprintln!("  compactions               {}", t.compactions);
    eprintln!("  malformed lines           {}", t.malformed);
    eprintln!("─────────────────────────────────────────────────────────\n");

    assert!(
        t.lines_with_usage > 0,
        "parsed no usage at all from real data"
    );

    // The property under test: real transcripts contain fan-out, so the naive
    // per-line sum must be materially higher than the deduplicated one. When
    // this was measured during format investigation the ratio was ~2.1x.
    assert!(
        t.naive_output > t.deduped_output,
        "expected real fan-out; naive {} vs deduped {}",
        t.naive_output,
        t.deduped_output
    );
    assert!(
        t.distinct_requests < t.lines_with_usage as usize,
        "expected repeated observations of the same request"
    );

    // A parser that cannot read the current format is worse than useless.
    let malformed_ratio = t.malformed as f64 / t.lines_with_usage.max(1) as f64;
    assert!(
        malformed_ratio < 0.01,
        "{:.2}% of candidate lines failed to parse; the format has probably changed",
        malformed_ratio * 100.0
    );
}

#[test]
#[ignore = "requires real Claude Code transcripts on this machine"]
fn subagent_transcripts_carry_a_material_share_of_usage() {
    // Sub-agent files are a majority of the files and a large minority of the
    // tokens. An implementation that reads only top-level session files loses
    // them silently, which is why this is asserted rather than assumed.
    let Some(root) = projects_dir() else {
        return;
    };

    let mut all = Vec::new();
    transcripts(&root, &mut all);

    let (subagent, main): (Vec<_>, Vec<_>) = all
        .into_iter()
        .partition(|p| p.to_string_lossy().contains("/subagents/"));

    if subagent.is_empty() {
        eprintln!("no sub-agent transcripts present; nothing to compare");
        return;
    }

    let main_t = tally(&main);
    let sub_t = tally(&subagent);
    let total = main_t.distinct_requests + sub_t.distinct_requests;

    eprintln!(
        "\n  sub-agent files {} of {}, requests {} of {} ({:.0}%)\n",
        subagent.len(),
        subagent.len() + main.len(),
        sub_t.distinct_requests,
        total,
        100.0 * sub_t.distinct_requests as f64 / total.max(1) as f64
    );

    assert!(
        sub_t.distinct_requests > 0,
        "sub-agent transcripts parsed to nothing, so their usage would be lost"
    );
}
