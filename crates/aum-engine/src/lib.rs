//! # `aum-engine` — tasks, ingest, and the live view
//!
//! Ties the pieces together: adapters produce signals, storage records them,
//! and this crate decides what a task currently looks like.

pub mod ingest;
pub mod metrics;
pub mod scan;
pub mod tasks;

use std::sync::Arc;
use std::time::Duration;

use aum_adapters::UsageAdapter;
use aum_adapters::claude_code::ClaudeCodeAdapter;
use aum_adapters::codex::CodexAdapter;
use aum_db::Database;
use tokio::sync::RwLock;

pub use ingest::{PassStats, WatchRoot, ingest_file, ingest_root};
pub use metrics::{Completeness, MetricsInput};
pub use scan::{ScanCache, ScanResult};
pub use tasks::{Agent, TaskError, TaskManager, TaskSpec};

/// How often files are re-checked while something is running.
///
/// Filesystem notifications on macOS are coalesced and directory-granular —
/// a hint, not a ledger — so an unconditional poll backs them up. Stat-ing a
/// few hundred files costs microseconds on APFS, and skipping it would buy
/// nothing except an entire class of "the live number is stale" bugs.
const ACTIVE_INTERVAL: Duration = Duration::from_millis(250);
/// How often they are re-checked when nothing is running.
const IDLE_INTERVAL: Duration = Duration::from_secs(2);

/// A snapshot of what ingest has done so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngestState {
    pub passes: u64,
    pub last: PassStats,
    pub cumulative: PassStats,
    /// True until the first pass over existing history has finished, so the UI
    /// can distinguish "nothing here" from "still reading".
    pub backfilling: bool,
}

/// Owns the adapters and the ingest loop.
pub struct Engine {
    db: Database,
    adapters: Vec<Box<dyn UsageAdapter>>,
    roots: Vec<WatchRoot>,
    state: Arc<RwLock<IngestState>>,
    /// One cache per root, so an idle pass costs a handful of `stat` calls
    /// rather than a full walk plus a database round-trip per file.
    caches: tokio::sync::Mutex<Vec<ScanCache>>,
}

impl Engine {
    /// Build an engine over the agents installed for a given home directory.
    #[must_use]
    pub fn new(db: Database, home: &std::path::Path) -> Self {
        Self {
            db,
            adapters: vec![Box::new(ClaudeCodeAdapter), Box::new(CodexAdapter)],
            roots: vec![WatchRoot::claude_code(home), WatchRoot::codex(home)],
            state: Arc::new(RwLock::new(IngestState {
                backfilling: true,
                ..Default::default()
            })),
            caches: tokio::sync::Mutex::new(vec![ScanCache::new(), ScanCache::new()]),
        }
    }

    #[must_use]
    pub fn state_handle(&self) -> Arc<RwLock<IngestState>> {
        Arc::clone(&self.state)
    }

    #[must_use]
    pub fn database(&self) -> Database {
        self.db.clone()
    }

    /// One pass over every adapter.
    ///
    /// Only files whose size or mtime moved are read. Everything the cache
    /// decides is a hint: a file wrongly judged unchanged is late, never lost,
    /// because cursors resume exactly where they stopped and dedup keys make a
    /// redundant read free.
    pub async fn pass(&self) -> PassStats {
        let mut total = PassStats::default();
        let mut caches = self.caches.lock().await;

        for (index, (adapter, root)) in self.adapters.iter().zip(self.roots.iter()).enumerate() {
            let Some(cache) = caches.get_mut(index) else {
                continue;
            };
            let matches = |path: &std::path::Path| root.matches_public(path);
            let scan = cache.changed_since_last(&root.directory, &matches);

            total.files_skipped = total.files_skipped.saturating_add(scan.unchanged);

            for path in &scan.changed {
                match ingest_file(&self.db, adapter.as_ref(), path).await {
                    Ok(stats) => total.merge_public(stats),
                    Err(e) => {
                        // One unreadable file must not stop the pass: a
                        // transcript may be mid-rotation or owned by someone
                        // else. It stays in the cache, so the next real change
                        // brings it back.
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "skipping a file this pass"
                        );
                    }
                }
            }
        }
        total
    }

    /// Run until cancelled.
    ///
    /// The first pass reads whatever history exists, which on a machine that
    /// has used these agents for months is a gigabyte of files — but under 1%
    /// of it survives the byte prefilter, so it is a bounded, one-off cost
    /// rather than something to defer or paginate.
    pub async fn run(self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        tracing::info!("ingest starting");

        loop {
            let stats = self.pass().await;
            {
                let mut state = self.state.write().await;
                state.passes = state.passes.saturating_add(1);
                state.last = stats;
                state.cumulative.merge_public(stats);
                if state.passes == 1 {
                    state.backfilling = false;
                    tracing::info!(
                        files = stats.files_scanned,
                        requests = stats.usage_recorded,
                        "backfill complete"
                    );
                }
            }

            // Poll faster while something is actively writing. The scan itself
            // is now cheap enough that the fast interval costs a few `stat`
            // calls rather than a full walk.
            let interval = if stats.usage_recorded > 0 {
                ACTIVE_INTERVAL
            } else {
                IDLE_INTERVAL
            };

            tokio::select! {
                () = tokio::time::sleep(interval) => {}
                _ = shutdown.changed() => break,
            }
        }

        tracing::info!("ingest stopped");
    }
}

impl PassStats {
    /// Accumulate another pass's numbers.
    pub fn merge_public(&mut self, other: Self) {
        self.files_scanned = self.files_scanned.saturating_add(other.files_scanned);
        self.files_skipped = self.files_skipped.saturating_add(other.files_skipped);
        self.lines_read = self.lines_read.saturating_add(other.lines_read);
        self.usage_recorded = self.usage_recorded.saturating_add(other.usage_recorded);
        self.duplicates = self.duplicates.saturating_add(other.duplicates);
        self.failures = self.failures.saturating_add(other.failures);
        self.retry_attempts = self.retry_attempts.saturating_add(other.retry_attempts);
        self.anomalies = self.anomalies.saturating_add(other.anomalies);
        self.oversize_relevant = self
            .oversize_relevant
            .saturating_add(other.oversize_relevant);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[tokio::test]
    async fn an_engine_over_an_empty_home_does_nothing_and_does_not_fail() {
        // A machine with neither agent installed must start cleanly rather than
        // erroring about missing directories.
        let db = aum_db::open_in_memory().await.unwrap();
        let home = tempfile::tempdir().unwrap();
        let engine = Engine::new(db, home.path());

        let stats = engine.pass().await;
        assert_eq!(stats.files_scanned, 0);
        assert_eq!(stats.usage_recorded, 0);
    }

    #[tokio::test]
    async fn a_pass_reads_both_agents() {
        let db = aum_db::open_in_memory().await.unwrap();
        let home = tempfile::tempdir().unwrap();

        let claude_dir = home.path().join(".claude").join("projects").join("proj");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("s.jsonl"),
            format!(
                "{}\n",
                r#"{"type":"assistant","sessionId":"c1","timestamp":"2026-08-12T19:10:15.774Z",
                   "requestId":"req_1","message":{"id":"msg_1","model":"claude-opus-5",
                   "usage":{"input_tokens":10,"output_tokens":20,
                   "cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#
                    .replace('\n', "")
            ),
        )
        .unwrap();

        let codex_dir = home.path().join(".codex").join("sessions").join("2026");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("rollout-x.jsonl"),
            format!(
                "{}\n{}\n",
                r#"{"type":"session_meta","payload":{"id":"x1","cwd":"/x"}}"#,
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{
                   "total_token_usage":{"input_tokens":100,"cached_input_tokens":0,
                   "output_tokens":50,"reasoning_output_tokens":5,"total_tokens":150},
                   "last_token_usage":{"input_tokens":100,"cached_input_tokens":0,
                   "output_tokens":50,"reasoning_output_tokens":5,"total_tokens":150}}}}"#
                    .replace('\n', "")
            ),
        )
        .unwrap();

        let engine = Engine::new(db, home.path());
        let stats = engine.pass().await;

        assert_eq!(stats.files_scanned, 2, "both agents' files");
        assert_eq!(stats.usage_recorded, 2, "one request each");

        // And they stay distinguishable: reasoning is reported by one and not
        // the other, which is the asymmetry the whole product must preserve.
        let rows: Vec<(String, Option<i64>)> =
            sqlx::query_as("SELECT adapter_id, reasoning FROM ai_request ORDER BY adapter_id")
                .fetch_all(engine.database().reader())
                .await
                .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.first().unwrap().0, "claude_code");
        assert_eq!(rows.first().unwrap().1, None, "Claude reports no reasoning");
        assert_eq!(rows.get(1).unwrap().0, "codex");
        assert_eq!(rows.get(1).unwrap().1, Some(5));
    }

    #[tokio::test]
    async fn repeated_passes_do_not_re_record_anything() {
        let db = aum_db::open_in_memory().await.unwrap();
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude").join("projects");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("s.jsonl"),
            format!(
                "{}\n",
                r#"{"type":"assistant","sessionId":"c1","timestamp":"2026-08-12T19:10:15.774Z",
                   "requestId":"req_1","message":{"id":"msg_1","model":"claude-opus-5",
                   "usage":{"input_tokens":10,"output_tokens":20,
                   "cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#
                    .replace('\n', "")
            ),
        )
        .unwrap();

        let engine = Engine::new(db, home.path());
        assert_eq!(engine.pass().await.usage_recorded, 1);
        assert_eq!(engine.pass().await.usage_recorded, 0, "nothing new to read");

        let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ai_request")
            .fetch_one(engine.database().reader())
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }
}

/// Build a task's current metrics from storage.
///
/// One place, used by both the REST snapshot and the pushed one, so the two can
/// never disagree — which matters because the stream is only ever a hint and
/// the client is expected to fall back to the GET.
pub async fn task_metrics(
    db: &Database,
    task_id: uuid::Uuid,
    table: &aum_pricing::PriceTable,
) -> Result<aum_contract::TaskMetrics, aum_db::DbError> {
    use aum_contract::{Currency, TaskStatus};

    let id = task_id.to_string();
    let totals = aum_db::repo::task_totals(db.reader(), &id).await?;
    let per_model = aum_db::repo::task_totals_by_model(db.reader(), &id).await?;

    let row: Option<(
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT status, adapter_id, model_id, started_at, ended_at FROM task WHERE id = ?1",
    )
    .bind(&id)
    .fetch_optional(db.reader())
    .await?;

    let (status, adapter_id, model_id, started_at, ended_at) =
        row.unwrap_or_else(|| ("pending".to_owned(), "unknown".to_owned(), None, None, None));

    let anomalies = aum_db::repo::anomaly_count_for_task(db.reader(), &id).await?;
    let failures = aum_db::repo::failed_request_count(db.reader(), &id).await?;

    let completeness = metrics::Completeness {
        unmeasured_requests: u32::try_from(failures).unwrap_or(u32::MAX),
        started_mid_session: false,
        anomalies: u32::try_from(anomalies).unwrap_or(u32::MAX),
    };

    // The model shown is whichever the requests actually used, falling back to
    // what the task recorded. `None` stays `None`: a task that has not yet made
    // a request genuinely has no model.
    let observed_model = per_model.iter().find_map(|(m, _)| m.clone()).or(model_id);

    let agent = tasks::Agent::parse(&adapter_id);

    Ok(metrics::build(&metrics::MetricsInput {
        task_id,
        status: match status.as_str() {
            "running" => TaskStatus::Running,
            "completed" => TaskStatus::Completed,
            "failed" => TaskStatus::Failed,
            "stopped" => TaskStatus::Stopped,
            _ => TaskStatus::Pending,
        },
        totals: &totals,
        completeness: &completeness,
        model_id: observed_model,
        elapsed_ms: elapsed_ms(started_at.as_deref(), ended_at.as_deref()),
        failed_requests: u32::try_from(failures).unwrap_or(u32::MAX),
        // Claude Code exposes retry attempts; Codex does not expose them at
        // all, and reporting 0 there would make it look flawless when it is
        // merely opaque.
        retries: match agent {
            Some(tasks::Agent::ClaudeCode) => Some(0),
            _ => None,
        },
        adapter_id: &adapter_id,
        currency: Currency::Usd,
        api_equivalent: metrics::cost_task(&per_model, table),
        subscription_plan: agent.map(|a| a.subscription_plan().to_owned()),
    }))
}

/// Wall-clock elapsed for a task, from its own timestamps.
///
/// Not a latency measurement and never used as one: it is the time the task
/// existed, which includes everything the agent did between requests.
fn elapsed_ms(started_at: Option<&str>, ended_at: Option<&str>) -> u64 {
    let Some(start) = started_at.and_then(parse_time) else {
        return 0;
    };
    let end = ended_at
        .and_then(parse_time)
        .unwrap_or_else(chrono::Utc::now);
    u64::try_from(end.signed_duration_since(start).num_milliseconds()).unwrap_or(0)
}

fn parse_time(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}
