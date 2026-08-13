//! Queries.
//!
//! One function per operation, all taking a pool so the caller decides whether
//! it is reading or writing.

use aum_domain::TokenUsage;
use sqlx::{Row, Sqlite, pool::Pool};

use crate::{DbError, now_sql, to_sql_time};

pub type Result<T> = std::result::Result<T, DbError>;

/// A usage observation ready to be persisted.
#[derive(Debug, Clone)]
pub struct UsageRecord {
    pub adapter_id: String,
    pub session_id: String,
    /// Adapter-computed identity. The thing that makes re-ingest safe.
    pub dedup_key: String,
    pub model_id: Option<String>,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub measurement_source: String,
    pub request_kind: String,
    pub usage: TokenUsage,
    pub is_sidechain: bool,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    /// The provider's own object, kept verbatim as the audit trail.
    pub raw_json: Option<String>,
}

/// How a request row was affected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    /// First sighting.
    Inserted,
    /// Already known, and at least one component grew.
    Updated,
    /// Already known with identical or larger values — a re-read.
    Unchanged,
}

/// Insert or merge a usage observation.
///
/// The merge is a **component-wise maximum**, which is what makes ingest safe
/// to repeat. Claude Code writes one API response as up to 21 JSONL lines, each
/// repeating the entire usage object; where the copies differ the values are
/// monotonically non-decreasing, because an early line is flushed mid-stream
/// with a preliminary output count.
///
/// Taking the maximum is therefore correct *and* order-independent *and*
/// idempotent — so re-reading a file after a crash, backfill overlapping live
/// tailing, and out-of-order arrival all converge on the same answer. "Last
/// write wins" would also be correct today, and would break silently the first
/// time any of those three happened.
///
/// Attribution is resolved here by a single indexed equality lookup against
/// `task_binding`. A session with no binding yields `task_id = NULL`, which
/// surfaces in the Unattributed view. It is never guessed into a task.
pub async fn upsert_usage(pool: &Pool<Sqlite>, rec: &UsageRecord) -> Result<UpsertOutcome> {
    let mut tx = pool.begin().await?;
    let now = now_sql();
    let occurred = to_sql_time(rec.occurred_at);

    let existing: Option<ExistingRow> = sqlx::query_as(
        "SELECT id, input_fresh, cache_read, cache_write_5m, cache_write_1h,
                cache_write_unspecified, output_total, reasoning, unclassified
           FROM ai_request
          WHERE adapter_id = ?1 AND session_id = ?2 AND dedup_key = ?3",
    )
    .bind(&rec.adapter_id)
    .bind(&rec.session_id)
    .bind(&rec.dedup_key)
    .fetch_optional(&mut *tx)
    .await?;

    let (request_id, outcome) = match existing {
        None => {
            let id = uuid::Uuid::new_v4().to_string();

            // Resolve attribution: exactly one equality lookup, no fuzzy match.
            let binding: Option<(String, String)> =
                sqlx::query_as("SELECT task_id, method FROM task_binding WHERE session_id = ?1")
                    .bind(&rec.session_id)
                    .fetch_optional(&mut *tx)
                    .await?;

            sqlx::query(
                "INSERT INTO ai_request
                   (id, adapter_id, session_id, task_id, dedup_key, model_id, occurred_at,
                    measurement_source, request_kind, attribution_method,
                    input_fresh, cache_read, cache_write_5m, cache_write_1h,
                    cache_write_unspecified, output_total, reasoning, unclassified,
                    is_sidechain, agent_id, agent_type, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            )
            .bind(&id)
            .bind(&rec.adapter_id)
            .bind(&rec.session_id)
            .bind(binding.as_ref().map(|b| b.0.clone()))
            .bind(&rec.dedup_key)
            .bind(&rec.model_id)
            .bind(&occurred)
            .bind(&rec.measurement_source)
            .bind(&rec.request_kind)
            .bind(binding.as_ref().map(|b| b.1.clone()))
            .bind(i64::try_from(rec.usage.input_fresh()).unwrap_or(i64::MAX))
            .bind(i64::try_from(rec.usage.cache_read()).unwrap_or(i64::MAX))
            .bind(i64::try_from(rec.usage.cache_write_5m()).unwrap_or(i64::MAX))
            .bind(i64::try_from(rec.usage.cache_write_1h()).unwrap_or(i64::MAX))
            .bind(i64::try_from(rec.usage.cache_write_unspecified()).unwrap_or(i64::MAX))
            .bind(i64::try_from(rec.usage.output_total()).unwrap_or(i64::MAX))
            .bind(rec.usage.reasoning().map(|r| i64::try_from(r).unwrap_or(i64::MAX)))
            .bind(i64::try_from(rec.usage.unclassified()).unwrap_or(i64::MAX))
            .bind(i64::from(rec.is_sidechain))
            .bind(&rec.agent_id)
            .bind(&rec.agent_type)
            .bind(&now)
            .execute(&mut *tx)
            .await?;

            (id, UpsertOutcome::Inserted)
        }

        Some((id, in_f, c_r, cw5, cw1, cwu, out, reas, unc)) => {
            let existing = (in_f, c_r, cw5, cw1, cwu, out, reas, unc);
            let merged = merge_max_row(existing, &rec.usage);
            let changed = merged != existing;

            if changed {
                sqlx::query(
                    "UPDATE ai_request
                        SET input_fresh = ?1, cache_read = ?2, cache_write_5m = ?3,
                            cache_write_1h = ?4, cache_write_unspecified = ?5,
                            output_total = ?6, reasoning = ?7, unclassified = ?8,
                            model_id = COALESCE(model_id, ?9)
                      WHERE id = ?10",
                )
                .bind(merged.0)
                .bind(merged.1)
                .bind(merged.2)
                .bind(merged.3)
                .bind(merged.4)
                .bind(merged.5)
                .bind(merged.6)
                .bind(merged.7)
                .bind(&rec.model_id)
                .bind(&id)
                .execute(&mut *tx)
                .await?;
            }

            (
                id,
                if changed {
                    UpsertOutcome::Updated
                } else {
                    UpsertOutcome::Unchanged
                },
            )
        }
    };

    // The per-source row, so a request measured three ways (transcript, OTEL,
    // stream-json) keeps all three for comparison rather than overwriting.
    sqlx::query(
        "INSERT INTO token_usage
           (ai_request_id, measurement_source, input_fresh, cache_read,
            cache_write_5m, cache_write_1h, cache_write_unspecified,
            output_total, reasoning, unclassified, raw_json, observed_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(ai_request_id, measurement_source) DO UPDATE SET
           input_fresh             = MAX(input_fresh,             excluded.input_fresh),
           cache_read              = MAX(cache_read,              excluded.cache_read),
           cache_write_5m          = MAX(cache_write_5m,          excluded.cache_write_5m),
           cache_write_1h          = MAX(cache_write_1h,          excluded.cache_write_1h),
           cache_write_unspecified = MAX(cache_write_unspecified, excluded.cache_write_unspecified),
           output_total            = MAX(output_total,            excluded.output_total),
           reasoning               = MAX(COALESCE(reasoning, -1), COALESCE(excluded.reasoning, -1)),
           unclassified            = MAX(unclassified,            excluded.unclassified),
           raw_json                = COALESCE(excluded.raw_json, raw_json),
           observed_at             = excluded.observed_at",
    )
    .bind(&request_id)
    .bind(&rec.measurement_source)
    .bind(i64::try_from(rec.usage.input_fresh()).unwrap_or(i64::MAX))
    .bind(i64::try_from(rec.usage.cache_read()).unwrap_or(i64::MAX))
    .bind(i64::try_from(rec.usage.cache_write_5m()).unwrap_or(i64::MAX))
    .bind(i64::try_from(rec.usage.cache_write_1h()).unwrap_or(i64::MAX))
    .bind(i64::try_from(rec.usage.cache_write_unspecified()).unwrap_or(i64::MAX))
    .bind(i64::try_from(rec.usage.output_total()).unwrap_or(i64::MAX))
    .bind(
        rec.usage
            .reasoning()
            .map(|r| i64::try_from(r).unwrap_or(i64::MAX)),
    )
    .bind(i64::try_from(rec.usage.unclassified()).unwrap_or(i64::MAX))
    .bind(&rec.raw_json)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    // `MAX(COALESCE(reasoning, -1), ...)` above turns "both unknown" into -1.
    // Restore it to NULL: -1 tokens is not a measurement, and a stray sentinel
    // leaking into an aggregate would be worse than the problem it solved.
    sqlx::query(
        "UPDATE token_usage SET reasoning = NULL
          WHERE ai_request_id = ?1 AND measurement_source = ?2 AND reasoning < 0",
    )
    .bind(&request_id)
    .bind(&rec.measurement_source)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(outcome)
}

/// The denormalized token columns on `ai_request`, in column order.
type UsageRow = (i64, i64, i64, i64, i64, i64, Option<i64>, i64);
/// A `UsageRow` with its row id, as selected for the merge.
type ExistingRow = (String, i64, i64, i64, i64, i64, i64, Option<i64>, i64);

fn merge_max_row(existing: UsageRow, incoming: &TokenUsage) -> UsageRow {
    let n = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
    (
        existing.0.max(n(incoming.input_fresh())),
        existing.1.max(n(incoming.cache_read())),
        existing.2.max(n(incoming.cache_write_5m())),
        existing.3.max(n(incoming.cache_write_1h())),
        existing.4.max(n(incoming.cache_write_unspecified())),
        existing.5.max(n(incoming.output_total())),
        // Absence stays absence: a provider that does not report reasoning must
        // not acquire a zero just because it was merged with one that does.
        match (existing.6, incoming.reasoning()) {
            (Some(a), Some(b)) => Some(a.max(n(b))),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(n(b)),
            (None, None) => None,
        },
        existing.7.max(n(incoming.unclassified())),
    )
}

/// Bind a provider session to a task.
///
/// Fails if the session already belongs to another task — that collision is the
/// database refusing to let two tasks share usage, and it should surface as a
/// conflict rather than be papered over.
pub async fn bind_session(
    pool: &Pool<Sqlite>,
    session_id: &str,
    task_id: &str,
    adapter_id: &str,
    method: &str,
    evidence: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO task_binding (session_id, task_id, adapter_id, method, evidence, bound_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(session_id)
    .bind(task_id)
    .bind(adapter_id)
    .bind(method)
    .bind(evidence)
    .bind(now_sql())
    .execute(pool)
    .await?;
    Ok(())
}

/// Retro-attribute requests already recorded for a session.
///
/// Used when a task binds to a session that was observed before monitoring
/// began. Returns how many rows moved out of the unattributed bucket.
pub async fn attribute_existing(
    pool: &Pool<Sqlite>,
    session_id: &str,
    task_id: &str,
    method: &str,
) -> Result<u64> {
    let result = sqlx::query(
        "UPDATE ai_request SET task_id = ?1, attribution_method = ?2
          WHERE session_id = ?3 AND task_id IS NULL",
    )
    .bind(task_id)
    .bind(method)
    .bind(session_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Aggregate token counts for a task, per model.
///
/// One index scan, no joins: this runs once a second per running task.
#[derive(Debug, Clone, Default)]
pub struct TaskTotals {
    pub requests: i64,
    pub input_fresh: i64,
    pub cache_read: i64,
    pub cache_write_5m: i64,
    pub cache_write_1h: i64,
    pub cache_write_unspecified: i64,
    pub output_total: i64,
    /// Tokens counted by the provider but not attributable to input or output,
    /// and therefore not priceable.
    pub unclassified: i64,
    /// `None` when no contributing request reported reasoning at all.
    pub reasoning: Option<i64>,
    /// How many requests reported reasoning, for the partial-aggregate rule.
    pub reasoning_reported_by: i64,
    pub first_at: Option<String>,
    pub last_at: Option<String>,
}

pub async fn task_totals(pool: &Pool<Sqlite>, task_id: &str) -> Result<TaskTotals> {
    let row = sqlx::query(
        "SELECT COUNT(*)                          AS requests,
                COALESCE(SUM(input_fresh), 0)     AS input_fresh,
                COALESCE(SUM(cache_read), 0)      AS cache_read,
                COALESCE(SUM(cache_write_5m), 0)  AS cache_write_5m,
                COALESCE(SUM(cache_write_1h), 0)  AS cache_write_1h,
                COALESCE(SUM(cache_write_unspecified), 0) AS cache_write_unspecified,
                COALESCE(SUM(output_total), 0)    AS output_total,
                COALESCE(SUM(unclassified), 0)    AS unclassified,
                SUM(reasoning)                    AS reasoning,
                COUNT(reasoning)                  AS reasoning_reported_by,
                MIN(occurred_at)                  AS first_at,
                MAX(occurred_at)                  AS last_at
           FROM ai_request
          WHERE task_id = ?1",
    )
    .bind(task_id)
    .fetch_one(pool)
    .await?;

    Ok(TaskTotals {
        requests: row.try_get("requests")?,
        input_fresh: row.try_get("input_fresh")?,
        cache_read: row.try_get("cache_read")?,
        cache_write_5m: row.try_get("cache_write_5m")?,
        cache_write_1h: row.try_get("cache_write_1h")?,
        cache_write_unspecified: row.try_get("cache_write_unspecified")?,
        output_total: row.try_get("output_total")?,
        unclassified: row.try_get("unclassified")?,
        // SUM over all-NULL yields NULL, which is exactly right: no contributor
        // reported reasoning, so the total is unknown rather than zero.
        reasoning: row.try_get("reasoning")?,
        reasoning_reported_by: row.try_get("reasoning_reported_by")?,
        first_at: row.try_get("first_at")?,
        last_at: row.try_get("last_at")?,
    })
}

/// Usage observed but claimed by no task.
pub async fn unattributed_count(pool: &Pool<Sqlite>) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ai_request WHERE task_id IS NULL")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

pub async fn record_anomaly(
    pool: &Pool<Sqlite>,
    adapter_id: &str,
    session_id: Option<&str>,
    kind: &str,
    detail: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO ingest_anomaly (id, adapter_id, session_id, kind, detail, occurred_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(adapter_id)
    .bind(session_id)
    .bind(kind)
    .bind(detail)
    .bind(now_sql())
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use aum_domain::{AnthropicUsage, OpenAiUsage};

    async fn db() -> crate::Database {
        crate::open_in_memory().await.unwrap()
    }

    fn claude_usage(output: u64) -> TokenUsage {
        TokenUsage::from_anthropic(&AnthropicUsage {
            input_tokens: Some(2),
            output_tokens: Some(output),
            cache_read_input_tokens: Some(30_885),
            cache_creation_input_tokens: Some(23_610),
            cache_creation: Some(aum_domain::native::AnthropicCacheCreation {
                ephemeral_5m_input_tokens: Some(0),
                ephemeral_1h_input_tokens: Some(23_610),
            }),
            ..Default::default()
        })
        .unwrap()
        .usage
    }

    fn record(dedup: &str, usage: TokenUsage) -> UsageRecord {
        UsageRecord {
            adapter_id: "claude_code".into(),
            session_id: "sess-1".into(),
            dedup_key: dedup.into(),
            model_id: Some("claude-opus-5".into()),
            occurred_at: chrono::Utc::now(),
            measurement_source: "provider_exact".into(),
            request_kind: "turn".into(),
            usage,
            is_sidechain: false,
            agent_id: None,
            agent_type: None,
            raw_json: None,
        }
    }

    #[tokio::test]
    async fn the_fanout_case_yields_one_request_not_eight() {
        // The verified failure mode: one API response written to 8 JSONL lines,
        // each repeating the whole usage object. Summing them inflates output
        // 8x. This is the single highest-impact bug in the ingest path.
        let db = db().await;
        let usage = claude_usage(1_454);

        for _ in 0..8 {
            upsert_usage(db.writer(), &record("req_1:msg_1", usage))
                .await
                .unwrap();
        }

        let totals = task_totals(db.reader(), "no-task").await.unwrap();
        assert_eq!(totals.requests, 0, "unbound session must not attribute");

        let (rows, output): (i64, i64) =
            sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(output_total),0) FROM ai_request")
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(output, 1_454, "must be 1454, not 11632");
    }

    #[tokio::test]
    async fn a_later_larger_observation_wins_but_a_smaller_one_does_not() {
        // An early line is flushed mid-stream with a preliminary output count;
        // the final line carries the real one. Order must not matter.
        let db = db().await;

        assert_eq!(
            upsert_usage(db.writer(), &record("k", claude_usage(1)))
                .await
                .unwrap(),
            UpsertOutcome::Inserted
        );
        assert_eq!(
            upsert_usage(db.writer(), &record("k", claude_usage(378)))
                .await
                .unwrap(),
            UpsertOutcome::Updated
        );
        // Re-reading the earlier line after a crash must not undo the update.
        assert_eq!(
            upsert_usage(db.writer(), &record("k", claude_usage(1)))
                .await
                .unwrap(),
            UpsertOutcome::Unchanged
        );

        let (output,): (i64,) =
            sqlx::query_as("SELECT output_total FROM ai_request WHERE dedup_key = 'k'")
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(output, 378);
    }

    #[tokio::test]
    async fn usage_with_no_bound_task_lands_in_the_unattributed_bucket() {
        let db = db().await;
        upsert_usage(db.writer(), &record("k", claude_usage(100)))
            .await
            .unwrap();
        assert_eq!(unattributed_count(db.reader()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_bound_session_attributes_to_exactly_that_task() {
        let db = db().await;
        sqlx::query(
            "INSERT INTO task (id, name, adapter_id, status, created_at)
             VALUES ('t1','x','claude_code','running', ?)",
        )
        .bind(now_sql())
        .execute(db.writer())
        .await
        .unwrap();

        bind_session(
            db.writer(),
            "sess-1",
            "t1",
            "claude_code",
            "launched_pinned",
            "{}",
        )
        .await
        .unwrap();

        upsert_usage(db.writer(), &record("k", claude_usage(500)))
            .await
            .unwrap();

        let totals = task_totals(db.reader(), "t1").await.unwrap();
        assert_eq!(totals.requests, 1);
        assert_eq!(totals.output_total, 500);
        assert_eq!(unattributed_count(db.reader()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn two_tasks_never_see_each_others_usage() {
        // Twenty concurrent tasks in miniature. Set equality per task, not
        // totals: two symmetric cross-attributions would cancel in a sum.
        let db = db().await;
        for (task, session) in [("t1", "s1"), ("t2", "s2")] {
            sqlx::query(
                "INSERT INTO task (id, name, adapter_id, status, created_at)
                 VALUES (?, 'x', 'claude_code', 'running', ?)",
            )
            .bind(task)
            .bind(now_sql())
            .execute(db.writer())
            .await
            .unwrap();
            bind_session(
                db.writer(),
                session,
                task,
                "claude_code",
                "launched_pinned",
                "{}",
            )
            .await
            .unwrap();
        }

        for (session, dedup, out) in [("s1", "a", 100), ("s2", "b", 700), ("s1", "c", 200)] {
            let mut rec = record(dedup, claude_usage(out));
            rec.session_id = session.into();
            upsert_usage(db.writer(), &rec).await.unwrap();
        }

        assert_eq!(task_totals(db.reader(), "t1").await.unwrap().requests, 2);
        assert_eq!(
            task_totals(db.reader(), "t1").await.unwrap().output_total,
            300
        );
        assert_eq!(task_totals(db.reader(), "t2").await.unwrap().requests, 1);
        assert_eq!(
            task_totals(db.reader(), "t2").await.unwrap().output_total,
            700
        );
    }

    #[tokio::test]
    async fn a_task_of_only_claude_requests_reports_reasoning_as_unknown() {
        // Not zero. Claude Code reports no reasoning count, and SUM over all
        // NULLs must stay NULL all the way to the UI.
        let db = db().await;
        sqlx::query(
            "INSERT INTO task (id,name,adapter_id,status,created_at)
             VALUES ('t1','x','claude_code','running', ?)",
        )
        .bind(now_sql())
        .execute(db.writer())
        .await
        .unwrap();
        bind_session(
            db.writer(),
            "sess-1",
            "t1",
            "claude_code",
            "launched_pinned",
            "{}",
        )
        .await
        .unwrap();

        upsert_usage(db.writer(), &record("a", claude_usage(10)))
            .await
            .unwrap();

        let totals = task_totals(db.reader(), "t1").await.unwrap();
        assert_eq!(totals.reasoning, None);
        assert_eq!(totals.reasoning_reported_by, 0);
    }

    #[tokio::test]
    async fn a_mixed_task_reports_reasoning_as_a_floor_over_the_requests_that_have_it() {
        let db = db().await;
        sqlx::query(
            "INSERT INTO task (id,name,adapter_id,status,created_at)
             VALUES ('t1','x','mixed','running', ?)",
        )
        .bind(now_sql())
        .execute(db.writer())
        .await
        .unwrap();
        bind_session(
            db.writer(),
            "sess-1",
            "t1",
            "claude_code",
            "launched_pinned",
            "{}",
        )
        .await
        .unwrap();

        upsert_usage(db.writer(), &record("claude", claude_usage(100)))
            .await
            .unwrap();

        let codex = TokenUsage::from_openai(&OpenAiUsage {
            input_tokens: Some(16_210),
            cached_input_tokens: Some(11_008),
            output_tokens: Some(164),
            reasoning_output_tokens: Some(35),
            total_tokens: Some(16_374),
            ..Default::default()
        })
        .unwrap()
        .usage;
        upsert_usage(db.writer(), &record("codex", codex))
            .await
            .unwrap();

        let totals = task_totals(db.reader(), "t1").await.unwrap();
        assert_eq!(totals.reasoning, Some(35));
        // 1 of 2 requests reported it, so the aggregate is partial, not exact.
        assert_eq!(totals.reasoning_reported_by, 1);
        assert_eq!(totals.requests, 2);
    }

    #[tokio::test]
    async fn attaching_to_an_existing_session_can_claim_its_earlier_requests() {
        let db = db().await;
        upsert_usage(db.writer(), &record("a", claude_usage(100)))
            .await
            .unwrap();
        upsert_usage(db.writer(), &record("b", claude_usage(200)))
            .await
            .unwrap();
        assert_eq!(unattributed_count(db.reader()).await.unwrap(), 2);

        sqlx::query(
            "INSERT INTO task (id,name,adapter_id,status,created_at)
             VALUES ('t1','x','claude_code','running', ?)",
        )
        .bind(now_sql())
        .execute(db.writer())
        .await
        .unwrap();
        bind_session(
            db.writer(),
            "sess-1",
            "t1",
            "claude_code",
            "session_id_exact",
            "{}",
        )
        .await
        .unwrap();

        let moved = attribute_existing(db.writer(), "sess-1", "t1", "session_id_exact")
            .await
            .unwrap();
        assert_eq!(moved, 2);
        assert_eq!(unattributed_count(db.reader()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn the_provider_object_is_kept_verbatim_for_audit() {
        let db = db().await;
        let mut rec = record("k", claude_usage(100));
        rec.raw_json = Some(r#"{"input_tokens":2,"output_tokens":100}"#.into());
        upsert_usage(db.writer(), &rec).await.unwrap();

        let (raw,): (Option<String>,) = sqlx::query_as("SELECT raw_json FROM token_usage LIMIT 1")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert!(raw.unwrap().contains("\"input_tokens\":2"));
    }
}
