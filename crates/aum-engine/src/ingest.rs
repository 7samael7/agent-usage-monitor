//! The ingest pass.
//!
//! Discover the files an adapter cares about, read whatever is new, parse it,
//! and record it. Deliberately a plain function rather than a long-lived task:
//! it can be called from a timer, from a file-change notification, or directly
//! from a test, and it behaves identically in all three.
//!
//! Everything here is safe to repeat. Cursors mean a pass reads only what is
//! new; dedup keys mean that re-reading a file after a crash converges rather
//! than doubling. Those two properties are what let the loop be this simple.

use std::path::{Path, PathBuf};

use aum_adapters::{LineCtx, ParseOutcome, Signal, UsageAdapter};
use aum_db::Database;
use aum_db::repo::{self, UpsertOutcome, UsageRecord};

/// What one pass over one adapter's files did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassStats {
    pub files_scanned: u32,
    /// Files examined and found unchanged, so not read at all. High relative to
    /// `files_scanned` is the healthy state.
    pub files_skipped: u32,
    pub lines_read: u64,
    pub usage_recorded: u64,
    /// Observations of a request already known. Expected and harmless — it is
    /// what makes re-reading safe — but a useful signal when tuning.
    pub duplicates: u64,
    pub failures: u64,
    pub retry_attempts: u64,
    pub anomalies: u64,
    /// Lines too large to materialize that looked like they carried usage.
    /// Non-zero means real data was skipped and must be surfaced.
    pub oversize_relevant: u64,
}

/// Where an adapter's data lives.
pub struct WatchRoot {
    pub directory: PathBuf,
    /// Extension to match, without the dot.
    pub extension: &'static str,
    /// Optional filename prefix, so Codex's `rollout-*` files are picked out
    /// without also reading its other JSONL bookkeeping.
    pub file_prefix: Option<&'static str>,
}

impl WatchRoot {
    #[must_use]
    pub fn claude_code(home: &Path) -> Self {
        Self {
            directory: home.join(".claude").join("projects"),
            extension: "jsonl",
            file_prefix: None,
        }
    }

    #[must_use]
    pub fn codex(home: &Path) -> Self {
        Self {
            directory: home.join(".codex").join("sessions"),
            extension: "jsonl",
            file_prefix: Some("rollout-"),
        }
    }

    /// Whether this root cares about a path.
    ///
    /// One rule, used by the one thing that walks the tree. It was previously
    /// reachable under two names for two different walkers, which is how a fix
    /// applied to one of them can appear to do nothing at all.
    #[must_use]
    pub fn matches(&self, path: &Path) -> bool {
        if path.extension().is_none_or(|e| e != self.extension) {
            return false;
        }
        match self.file_prefix {
            None => true,
            Some(prefix) => path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(prefix)),
        }
    }
}

/// Read whatever is new in one file and record it.
pub async fn ingest_file(
    db: &Database,
    adapter: &dyn UsageAdapter,
    path: &Path,
) -> anyhow::Result<PassStats> {
    let mut stats = PassStats {
        files_scanned: 1,
        ..Default::default()
    };

    let file_id = aum_ingest::FileId::of(path)?;
    let db_file_id = repo::register_file(
        db.writer(),
        adapter.id(),
        file_id.device,
        file_id.inode,
        &path.to_string_lossy(),
    )
    .await?;

    let stored = repo::load_cursor(db.reader(), &db_file_id).await?;
    let cursor = aum_ingest::Cursor {
        byte_offset: u64::try_from(stored.byte_offset).unwrap_or(0),
        line_ordinal: u64::try_from(stored.line_ordinal).unwrap_or(0),
        size_seen: u64::try_from(stored.size_seen).unwrap_or(0),
    };

    // Adapter state travels with the cursor, so Codex resumes its cumulative
    // watermark rather than re-deriving it from the top of the file.
    let mut ctx: LineCtx = stored
        .adapter_state
        .as_deref()
        .and_then(|s| serde_json::from_str::<PersistedState>(s).ok())
        .map(Into::into)
        .unwrap_or_default();
    ctx.event_ordinal = cursor.line_ordinal;

    // Collected during the synchronous read, then applied. The parser is pure,
    // so it cannot await; keeping that boundary explicit is what allows backfill
    // to run on a blocking pool later without restructuring anything.
    let mut signals = Vec::new();

    let (new_cursor, read_stats) = aum_ingest::read_new_lines(
        path,
        cursor,
        |head| adapter.is_candidate_line(head),
        |line| match adapter.parse_line(&mut ctx, line.bytes) {
            ParseOutcome::Signals(s) => signals.extend(s),
            ParseOutcome::Malformed { reason } => signals.push(Signal::Anomaly {
                kind: "malformed_line".to_owned(),
                detail: reason,
            }),
            ParseOutcome::Ignored => {}
        },
    )?;

    stats.lines_read = read_stats.scanned;
    stats.oversize_relevant = read_stats.oversize_relevant;

    if read_stats.oversize_relevant > 0 {
        repo::record_anomaly(
            db.writer(),
            adapter.id(),
            ctx.current_session.as_deref(),
            "oversize_relevant_line",
            &format!(
                "{} line(s) exceeded the size limit and were skipped despite looking like they \
                 carried usage; this task's totals are incomplete",
                read_stats.oversize_relevant
            ),
        )
        .await?;
        stats.anomalies = stats.anomalies.saturating_add(read_stats.oversize_relevant);
    }

    for signal in signals {
        match signal {
            Signal::Usage(u) => {
                let outcome = repo::upsert_usage(
                    db.writer(),
                    &UsageRecord {
                        adapter_id: adapter.id().to_owned(),
                        session_id: u.session_id,
                        dedup_key: u.dedup_key,
                        model_id: u.model_id,
                        occurred_at: u.occurred_at,
                        measurement_source: u.measurement_source.to_owned(),
                        request_kind: u.request_kind.to_owned(),
                        usage: u.usage,
                        is_sidechain: u.is_sidechain,
                        agent_id: u.agent_id,
                        agent_type: u.agent_type,
                        raw_json: u.raw_json,
                    },
                )
                .await?;

                match outcome {
                    UpsertOutcome::Inserted => stats.usage_recorded += 1,
                    UpsertOutcome::Updated | UpsertOutcome::Unchanged => stats.duplicates += 1,
                }
            }

            Signal::RequestFailed {
                session_id,
                dedup_key,
                occurred_at,
                detail,
            } => {
                // Persisted, not merely counted. A request that happened and
                // could not be measured is what stops a task's total calling
                // itself exact, and that only works if it is recorded.
                stats.failures += 1;
                repo::record_failure(
                    db.writer(),
                    adapter.id(),
                    &session_id,
                    &dedup_key,
                    occurred_at,
                    &detail,
                )
                .await?;
            }
            Signal::RetryAttempt { .. } => stats.retry_attempts += 1,

            Signal::Anomaly { kind, detail } => {
                stats.anomalies += 1;
                repo::record_anomaly(
                    db.writer(),
                    adapter.id(),
                    ctx.current_session.as_deref(),
                    &kind,
                    &detail,
                )
                .await?;
            }

            // Context annotations and session/model declarations carry no
            // tokens. Compaction in particular reports context-window sizes
            // near a million, which must never reach a usage total.
            Signal::SessionOpened { .. }
            | Signal::ModelDeclared { .. }
            | Signal::ContextCompacted { .. } => {}
        }
    }

    repo::save_cursor(
        db.writer(),
        &repo::FileCursor {
            file_id: db_file_id,
            byte_offset: i64::try_from(new_cursor.byte_offset).unwrap_or(i64::MAX),
            line_ordinal: i64::try_from(new_cursor.line_ordinal).unwrap_or(i64::MAX),
            size_seen: i64::try_from(new_cursor.size_seen).unwrap_or(i64::MAX),
            adapter_state: serde_json::to_string(&PersistedState::from(&ctx)).ok(),
        },
    )
    .await?;

    Ok(stats)
}

/// The parts of `LineCtx` worth carrying between passes.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct PersistedState {
    current_model: Option<String>,
    current_session: Option<String>,
    previous_cumulative: Option<aum_domain::OpenAiUsage>,
    previous_last: Option<aum_domain::OpenAiUsage>,
}

impl From<&LineCtx> for PersistedState {
    fn from(ctx: &LineCtx) -> Self {
        Self {
            current_model: ctx.current_model.clone(),
            current_session: ctx.current_session.clone(),
            previous_cumulative: ctx.previous_cumulative,
            previous_last: ctx.previous_last,
        }
    }
}

impl From<PersistedState> for LineCtx {
    fn from(s: PersistedState) -> Self {
        Self {
            current_model: s.current_model,
            current_session: s.current_session,
            event_ordinal: 0,
            previous_cumulative: s.previous_cumulative,
            previous_last: s.previous_last,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use aum_adapters::claude_code::ClaudeCodeAdapter;
    use std::io::Write as _;

    fn assistant(request_id: &str, message_id: &str, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"sess-1","uuid":"u-{request_id}",
              "timestamp":"2026-08-12T19:10:15.774Z","requestId":"{request_id}",
              "message":{{"id":"{message_id}","model":"claude-opus-5",
                "usage":{{"input_tokens":2,"output_tokens":{output},
                  "cache_creation_input_tokens":0,"cache_read_input_tokens":100}}}}}}"#
        )
        .replace('\n', "")
    }

    fn write(path: &Path, lines: &[String]) {
        let mut f = std::fs::File::create(path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    fn append(path: &Path, lines: &[String]) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    async fn totals(db: &Database) -> (i64, i64) {
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(output_total),0) FROM ai_request")
            .fetch_one(db.reader())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_pass_records_what_it_reads() {
        let db = aum_db::open_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        write(
            &path,
            &[
                assistant("req_1", "msg_1", 100),
                assistant("req_2", "msg_2", 200),
            ],
        );

        let stats = ingest_file(&db, &ClaudeCodeAdapter, &path).await.unwrap();
        assert_eq!(stats.usage_recorded, 2);
        assert_eq!(totals(&db).await, (2, 300));
    }

    #[tokio::test]
    async fn a_second_pass_reads_only_what_is_new() {
        let db = aum_db::open_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        write(&path, &[assistant("req_1", "msg_1", 100)]);

        ingest_file(&db, &ClaudeCodeAdapter, &path).await.unwrap();
        append(&path, &[assistant("req_2", "msg_2", 200)]);

        let second = ingest_file(&db, &ClaudeCodeAdapter, &path).await.unwrap();
        assert_eq!(second.usage_recorded, 1, "only the appended line");
        assert_eq!(totals(&db).await, (2, 300));
    }

    #[tokio::test]
    async fn re_reading_a_whole_file_does_not_double_anything() {
        // The crash-recovery property, end to end: cursor lost, file re-read
        // from the beginning, totals unchanged.
        let db = aum_db::open_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        write(
            &path,
            &[
                assistant("req_1", "msg_1", 100),
                assistant("req_2", "msg_2", 200),
            ],
        );

        ingest_file(&db, &ClaudeCodeAdapter, &path).await.unwrap();
        let before = totals(&db).await;

        // Simulate losing the cursor, as a crash before it was written would.
        sqlx::query("DELETE FROM ingest_cursor")
            .execute(db.writer())
            .await
            .unwrap();

        let again = ingest_file(&db, &ClaudeCodeAdapter, &path).await.unwrap();
        assert_eq!(
            totals(&db).await,
            before,
            "re-ingest must converge, not accumulate"
        );
        assert_eq!(again.usage_recorded, 0);
        assert_eq!(again.duplicates, 2);
    }

    #[tokio::test]
    async fn the_fanout_of_one_response_becomes_one_request() {
        // Eight lines, one API response — the 2.4x bug, through the full path.
        let db = aum_db::open_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let lines: Vec<String> = (0..8).map(|_| assistant("req_1", "msg_1", 1_454)).collect();
        write(&path, &lines);

        ingest_file(&db, &ClaudeCodeAdapter, &path).await.unwrap();
        assert_eq!(totals(&db).await, (1, 1_454), "must be 1454, not 11632");
    }

    #[tokio::test]
    async fn a_half_written_line_is_picked_up_once_it_completes() {
        let db = aum_db::open_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");

        // A line without its terminating newline, as a live writer leaves it.
        let partial = assistant("req_1", "msg_1", 100);
        let (head, tail) = partial.split_at(partial.len() / 2);
        std::fs::write(&path, head).unwrap();

        let first = ingest_file(&db, &ClaudeCodeAdapter, &path).await.unwrap();
        assert_eq!(
            first.usage_recorded, 0,
            "an incomplete record must not be parsed"
        );

        append(&path, &[tail.to_owned()]);
        let second = ingest_file(&db, &ClaudeCodeAdapter, &path).await.unwrap();
        assert_eq!(second.usage_recorded, 1);
    }

    /// What the running engine would actually pick up under a root.
    ///
    /// Deliberately routed through `ScanCache`, which is the only thing that
    /// walks the tree in production. A second walker used only by tests would
    /// let these keep passing while the real one lost files.
    fn discovered(root: &WatchRoot) -> Vec<PathBuf> {
        let matches = |path: &Path| root.matches(path);
        crate::scan::ScanCache::new()
            .changed_since_last(&root.directory, &matches)
            .changed
    }

    #[tokio::test]
    async fn discovery_finds_nested_subagent_transcripts() {
        // Sub-agent files are the majority of files and a third of the requests.
        // A non-recursive scan loses them silently.
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("session-1").join("subagents");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.path().join("session-1.jsonl"), "").unwrap();
        std::fs::write(nested.join("agent-1.jsonl"), "").unwrap();

        let root = WatchRoot {
            directory: dir.path().to_path_buf(),
            extension: "jsonl",
            file_prefix: None,
        };
        let found = discovered(&root);
        assert_eq!(found.len(), 2, "found {found:?}");
    }

    #[tokio::test]
    async fn a_prefix_filter_ignores_a_directorys_other_jsonl_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rollout-2026-01-01-abc.jsonl"), "").unwrap();
        std::fs::write(dir.path().join("history.jsonl"), "").unwrap();

        let root = WatchRoot {
            directory: dir.path().to_path_buf(),
            extension: "jsonl",
            file_prefix: Some("rollout-"),
        };
        let found = discovered(&root);
        assert_eq!(found.len(), 1);
        assert!(
            found
                .first()
                .unwrap()
                .to_string_lossy()
                .contains("rollout-")
        );
    }

    #[tokio::test]
    async fn usage_from_an_unbound_session_is_kept_and_marked_unattributed() {
        // Never guessed into a task, never dropped.
        let db = aum_db::open_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        write(&path, &[assistant("req_1", "msg_1", 100)]);

        ingest_file(&db, &ClaudeCodeAdapter, &path).await.unwrap();
        assert_eq!(repo::unattributed_count(db.reader()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_bound_session_attributes_to_its_task() {
        let db = aum_db::open_in_memory().await.unwrap();
        sqlx::query(
            "INSERT INTO task (id,name,adapter_id,status,created_at)
             VALUES ('t1','x','claude_code','running', ?)",
        )
        .bind(aum_db::now_sql())
        .execute(db.writer())
        .await
        .unwrap();
        repo::bind_session(
            db.writer(),
            "sess-1",
            "t1",
            "claude_code",
            "launched_pinned",
            "{}",
        )
        .await
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        write(&path, &[assistant("req_1", "msg_1", 500)]);
        ingest_file(&db, &ClaudeCodeAdapter, &path).await.unwrap();

        let totals = repo::task_totals(db.reader(), "t1").await.unwrap();
        assert_eq!(totals.requests, 1);
        assert_eq!(totals.output_total, 500);
    }

    #[tokio::test]
    async fn a_malformed_line_is_recorded_as_an_anomaly() {
        let db = aum_db::open_in_memory().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        // Passes the prefilter, fails to parse — the case that must be visible
        // rather than silently skipped.
        write(
            &path,
            &[r#"{"type":"assistant","usage":{ broken"#.to_owned()],
        );

        let stats = ingest_file(&db, &ClaudeCodeAdapter, &path).await.unwrap();
        assert_eq!(stats.anomalies, 1);

        let (recorded,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ingest_anomaly")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(recorded, 1);
    }
}
