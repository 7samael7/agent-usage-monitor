//! # `aum-engine` — tasks, ingest, and the live view
//!
//! Ties the pieces together: adapters produce signals, storage records them,
//! and this crate decides what a task currently looks like.

pub mod agents;
pub mod desktop;
pub mod ingest;
pub mod prices;
pub mod probe;
pub mod scan;
pub mod views;

use std::sync::Arc;
use std::time::Duration;

use aum_adapters::UsageAdapter;
use aum_adapters::claude_code::ClaudeCodeAdapter;
use aum_adapters::codex::CodexAdapter;
use aum_adapters::copilot::CopilotAdapter;
use aum_db::Database;
use tokio::sync::RwLock;

pub use ingest::{PassStats, WatchRoot, ingest_file};
pub use probe::{daily_total_from, describe_claude_desktop, describe_file_adapter, probe};
pub use scan::{ScanCache, ScanResult};

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
    /// When the last pass finished, so an interface can say how current its
    /// numbers are instead of implying they are current because it redrew.
    /// `None` until one has completed.
    pub last_pass_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Owns the adapters and the ingest loop.
pub struct Engine {
    db: Database,
    adapters: Vec<Box<dyn UsageAdapter>>,
    roots: Vec<WatchRoot>,
    /// Where the user's files live, for the handful of things read outside a
    /// watched root — Claude Desktop's daily counter, which is one small file.
    home: std::path::PathBuf,
    state: Arc<RwLock<IngestState>>,
    /// One cache per root, so an idle pass costs a handful of `stat` calls
    /// rather than a full walk plus a database round-trip per file.
    caches: tokio::sync::Mutex<Vec<ScanCache>>,
    /// Cuts the wait short when someone asks for a check now.
    wake: Arc<tokio::sync::Notify>,
}

impl Engine {
    /// Build an engine over the agents installed for a given home directory.
    #[must_use]
    pub fn new(db: Database, home: &std::path::Path) -> Self {
        Self {
            db,
            adapters: vec![
                Box::new(ClaudeCodeAdapter),
                Box::new(CodexAdapter),
                Box::new(CopilotAdapter),
            ],
            roots: vec![
                WatchRoot::claude_code(home),
                WatchRoot::codex(home),
                WatchRoot::copilot(home),
            ],
            home: home.to_path_buf(),
            state: Arc::new(RwLock::new(IngestState {
                backfilling: true,
                ..Default::default()
            })),
            // One cache per root, and the count must track `roots` above:
            // a missing entry silently stops that adapter being scanned.
            caches: tokio::sync::Mutex::new(
                std::iter::repeat_with(ScanCache::new).take(3).collect(),
            ),
            wake: Arc::new(tokio::sync::Notify::new()),
        }
    }

    #[must_use]
    pub fn state_handle(&self) -> Arc<RwLock<IngestState>> {
        Arc::clone(&self.state)
    }

    /// Ask the running loop to check the transcripts now rather than at the end
    /// of its interval.
    ///
    /// Notifying this rather than building a second `Engine` matters: a fresh
    /// engine starts with an empty scan cache and would re-read every file on
    /// the machine to discover that nothing had changed.
    #[must_use]
    pub fn wake_handle(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.wake)
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
            let matches = |path: &std::path::Path| root.matches(path);
            let scan = cache.changed_since_last(&root.directory, &matches);

            total.files_skipped = total.files_skipped.saturating_add(scan.unchanged);

            for path in &scan.changed {
                match ingest_file(&self.db, adapter.as_ref(), path).await {
                    Ok(stats) => total.merge(stats),
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

        // Claude Desktop keeps one running total for the current day and drops
        // it at midnight. Sampling it here is a 69-byte read, and it is the only
        // way the history survives. Recorded well away from the request tables:
        // it has no model, no input/output split and no request boundary, so it
        // must never reach a per-task or per-model total.
        if let Err(e) = desktop::sample(&self.db, &self.home).await {
            tracing::warn!(error = %e, "could not record Claude Desktop's daily counter");
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
        let wake = Arc::clone(&self.wake);

        loop {
            let stats = self.pass().await;
            {
                let mut state = self.state.write().await;
                state.passes = state.passes.saturating_add(1);
                state.last = stats;
                state.cumulative.merge(stats);
                state.last_pass_at = Some(chrono::Utc::now());
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
                // `Notify` holds one permit, so a wake sent while a pass was
                // running is honoured at the end of it rather than lost.
                () = wake.notified() => {}
                _ = shutdown.changed() => break,
            }
        }

        tracing::info!("ingest stopped");
    }
}

impl PassStats {
    /// Accumulate another pass's numbers.
    pub fn merge(&mut self, other: Self) {
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

    /// Every watch root needs its own adapter and its own scan cache: `pass`
    /// zips them together, so a short list silently stops the last adapter
    /// being scanned at all — no error, just an agent that never reports.
    #[tokio::test]
    async fn every_watch_root_has_an_adapter_and_a_cache() {
        let db = aum_db::open_in_memory().await.unwrap();
        let home = tempfile::tempdir().unwrap();
        let engine = Engine::new(db, home.path());

        assert_eq!(engine.adapters.len(), engine.roots.len());
        assert_eq!(engine.caches.lock().await.len(), engine.roots.len());
        assert!(
            engine.roots.iter().any(|r| r.directory.ends_with("otel")),
            "Copilot's export directory should be watched"
        );
    }

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
