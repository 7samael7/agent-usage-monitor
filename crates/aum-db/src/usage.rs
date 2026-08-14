//! Usage aggregated over time, across everything recorded.
//!
//! The task-scoped queries this replaces asked "what did this one run cost".
//! A terminal tool asks "what have I spent this week", which is a different
//! shape: no task, no session, a date range, and buckets.
//!
//! **Everything here groups by model as well as by time, even when the caller
//! only wants a daily total.** Rates differ between models by up to ten times,
//! so a day's tokens summed first and priced afterwards is priced at a blend
//! that matches no actual rate. Cost is computed per model and then added up;
//! the shape of these return types is what forces that.
//!
//! Failed requests are excluded from counts and sums throughout, and reported
//! separately. A failed call has no tokens — folding it in would overstate the
//! successes while making the two numbers contradict each other.

use sqlx::{Row, Sqlite, pool::Pool};

use crate::repo::Result;

/// Which slice of history to read.
///
/// All three are optional and independent. `since`/`until` are ISO-8601 UTC
/// strings compared lexically, which works because `to_sql_time` writes a fixed
/// width — the same reason the cursor timestamps sort correctly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub since: Option<String>,
    pub until: Option<String>,
    pub adapter: Option<String>,
}

impl Filter {
    /// Everything ever recorded.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn since(mut self, at: impl Into<String>) -> Self {
        self.since = Some(at.into());
        self
    }

    #[must_use]
    pub fn until(mut self, at: impl Into<String>) -> Self {
        self.until = Some(at.into());
        self
    }

    #[must_use]
    pub fn adapter(mut self, id: impl Into<String>) -> Self {
        self.adapter = Some(id.into());
        self
    }

    /// The `WHERE` fragment, as bindable placeholders.
    ///
    /// Built as `?`-placeholders rather than interpolated values: these strings
    /// come from a command line, and a query built by concatenation is one bad
    /// argument away from being a different query.
    fn predicate(&self) -> String {
        let mut sql = String::from(" WHERE request_kind != 'failed'");
        if self.since.is_some() {
            sql.push_str(" AND occurred_at >= ?");
        }
        if self.until.is_some() {
            sql.push_str(" AND occurred_at < ?");
        }
        if self.adapter.is_some() {
            sql.push_str(" AND adapter_id = ?");
        }
        sql
    }

    fn bind<'q>(
        &'q self,
        mut q: sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    ) -> sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
        if let Some(v) = &self.since {
            q = q.bind(v);
        }
        if let Some(v) = &self.until {
            q = q.bind(v);
        }
        if let Some(v) = &self.adapter {
            q = q.bind(v);
        }
        q
    }
}

/// A slice of usage: the disjoint token bands plus how many requests produced
/// them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub requests: i64,
    pub input_fresh: i64,
    pub cache_read: i64,
    pub cache_write_5m: i64,
    pub cache_write_1h: i64,
    pub cache_write_unspecified: i64,
    pub output_total: i64,
    pub unclassified: i64,
    /// `None` when no contributing request reported a reasoning count. Never
    /// `Some(0)` for an agent that does not report them — that would assert no
    /// reasoning happened.
    pub reasoning: Option<i64>,
    /// How many of `requests` carried a reasoning count, so a caller can tell
    /// a complete total from a floor.
    pub reasoning_reported_by: i64,
}

impl Totals {
    /// Every token, across every disjoint band.
    #[must_use]
    pub const fn grand_total(&self) -> i64 {
        self.input_fresh
            + self.cache_read
            + self.cache_write_5m
            + self.cache_write_1h
            + self.cache_write_unspecified
            + self.output_total
            + self.unclassified
    }

    /// Everything charged as input: fresh, cache reads and cache writes.
    ///
    /// The only input quantity that means the same thing for both providers —
    /// Anthropic excludes cached tokens from its input field and OpenAI
    /// includes them, so neither raw number is comparable on its own.
    #[must_use]
    pub const fn input_side(&self) -> i64 {
        self.input_fresh
            + self.cache_read
            + self.cache_write_5m
            + self.cache_write_1h
            + self.cache_write_unspecified
    }

    fn add(&mut self, other: &Self) {
        self.requests += other.requests;
        self.input_fresh += other.input_fresh;
        self.cache_read += other.cache_read;
        self.cache_write_5m += other.cache_write_5m;
        self.cache_write_1h += other.cache_write_1h;
        self.cache_write_unspecified += other.cache_write_unspecified;
        self.output_total += other.output_total;
        self.unclassified += other.unclassified;
        self.reasoning_reported_by += other.reasoning_reported_by;
        // Absence stays absence: a bucket where nobody reported reasoning must
        // not acquire a zero by being added to one where somebody did.
        self.reasoning = match (self.reasoning, other.reasoning) {
            (Some(a), Some(b)) => Some(a + b),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
    }
}

/// Sum a set of per-model slices into one.
#[must_use]
pub fn fold<'a>(parts: impl IntoIterator<Item = &'a Totals>) -> Totals {
    let mut out = Totals::default();
    for p in parts {
        out.add(p);
    }
    out
}

const SELECT_TOTALS: &str = "COUNT(*)                                  AS requests,
     COALESCE(SUM(input_fresh), 0)             AS input_fresh,
     COALESCE(SUM(cache_read), 0)              AS cache_read,
     COALESCE(SUM(cache_write_5m), 0)          AS cache_write_5m,
     COALESCE(SUM(cache_write_1h), 0)          AS cache_write_1h,
     COALESCE(SUM(cache_write_unspecified), 0) AS cache_write_unspecified,
     COALESCE(SUM(output_total), 0)            AS output_total,
     COALESCE(SUM(unclassified), 0)            AS unclassified,
     SUM(reasoning)                            AS reasoning,
     COUNT(reasoning)                          AS reasoning_reported_by";

fn read_totals(row: &sqlx::sqlite::SqliteRow) -> Result<Totals> {
    Ok(Totals {
        requests: row.try_get("requests")?,
        input_fresh: row.try_get("input_fresh")?,
        cache_read: row.try_get("cache_read")?,
        cache_write_5m: row.try_get("cache_write_5m")?,
        cache_write_1h: row.try_get("cache_write_1h")?,
        cache_write_unspecified: row.try_get("cache_write_unspecified")?,
        output_total: row.try_get("output_total")?,
        unclassified: row.try_get("unclassified")?,
        reasoning: row.try_get("reasoning")?,
        reasoning_reported_by: row.try_get("reasoning_reported_by")?,
    })
}

/// Grand totals over the filtered range.
pub async fn totals(pool: &Pool<Sqlite>, filter: &Filter) -> Result<Totals> {
    let sql = format!(
        "SELECT {SELECT_TOTALS} FROM ai_request{}",
        filter.predicate()
    );
    let row = filter.bind(sqlx::query(&sql)).fetch_one(pool).await?;
    read_totals(&row)
}

/// How many requests in the range failed and could not be measured.
pub async fn failed_count(pool: &Pool<Sqlite>, filter: &Filter) -> Result<i64> {
    // The filter's own predicate excludes failures, which is the point of it
    // everywhere else — so this asks the inverse question directly.
    let mut sql = String::from("SELECT COUNT(*) FROM ai_request WHERE request_kind = 'failed'");
    if filter.since.is_some() {
        sql.push_str(" AND occurred_at >= ?");
    }
    if filter.until.is_some() {
        sql.push_str(" AND occurred_at < ?");
    }
    if filter.adapter.is_some() {
        sql.push_str(" AND adapter_id = ?");
    }
    let row = filter.bind(sqlx::query(&sql)).fetch_one(pool).await?;
    Ok(row.try_get(0)?)
}

/// One bucket of one model's usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slice {
    /// Bucket label: `YYYY-MM-DD` for days, `YYYY-MM-DDTHH` for hours.
    pub at: String,
    /// `None` where the transcript never named a model. Not guessed.
    pub model_id: Option<String>,
    pub totals: Totals,
}

async fn grouped(pool: &Pool<Sqlite>, filter: &Filter, fmt: &str) -> Result<Vec<Slice>> {
    let sql = format!(
        "SELECT strftime('{fmt}', occurred_at) AS at, model_id, {SELECT_TOTALS}
           FROM ai_request{}
          GROUP BY at, model_id
          ORDER BY at",
        filter.predicate()
    );
    let rows = filter.bind(sqlx::query(&sql)).fetch_all(pool).await?;
    rows.iter()
        .map(|r| {
            Ok(Slice {
                at: r.try_get("at")?,
                model_id: r.try_get("model_id")?,
                totals: read_totals(r)?,
            })
        })
        .collect()
}

/// Per day, per model.
///
/// The primary aggregation: a daily cost is the sum of each model's cost that
/// day, never the day's blended tokens at one rate.
pub async fn by_day_model(pool: &Pool<Sqlite>, filter: &Filter) -> Result<Vec<Slice>> {
    grouped(pool, filter, "%Y-%m-%d").await
}

/// Per hour, per model.
pub async fn by_hour_model(pool: &Pool<Sqlite>, filter: &Filter) -> Result<Vec<Slice>> {
    grouped(pool, filter, "%Y-%m-%dT%H").await
}

/// Per model over the whole range, busiest first.
pub async fn by_model(
    pool: &Pool<Sqlite>,
    filter: &Filter,
) -> Result<Vec<(Option<String>, Totals)>> {
    let sql = format!(
        "SELECT model_id, {SELECT_TOTALS} FROM ai_request{} GROUP BY model_id ORDER BY requests DESC",
        filter.predicate()
    );
    let rows = filter.bind(sqlx::query(&sql)).fetch_all(pool).await?;
    rows.iter()
        .map(|r| Ok((r.try_get("model_id")?, read_totals(r)?)))
        .collect()
}

/// Per agent over the whole range.
pub async fn by_adapter(pool: &Pool<Sqlite>, filter: &Filter) -> Result<Vec<(String, Totals)>> {
    let sql = format!(
        "SELECT adapter_id, {SELECT_TOTALS} FROM ai_request{} GROUP BY adapter_id ORDER BY requests DESC",
        filter.predicate()
    );
    let rows = filter.bind(sqlx::query(&sql)).fetch_all(pool).await?;
    rows.iter()
        .map(|r| Ok((r.try_get("adapter_id")?, read_totals(r)?)))
        .collect()
}

/// Fold per-model slices into one row per bucket, preserving order.
///
/// For charts, which plot a day rather than a day's models. Cost must still be
/// derived from the unfolded slices.
#[must_use]
pub fn fold_by_bucket(slices: &[Slice]) -> Vec<(String, Totals)> {
    let mut out: Vec<(String, Totals)> = Vec::new();
    for s in slices {
        match out.last_mut() {
            Some((at, totals)) if *at == s.at => totals.add(&s.totals),
            _ => out.push((s.at.clone(), s.totals)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::repo::{UsageRecord, upsert_usage};
    use aum_domain::TokenUsage;

    async fn db() -> crate::Database {
        crate::open_in_memory().await.unwrap()
    }

    fn usage(input: u64, output: u64) -> TokenUsage {
        TokenUsage::from_bands(aum_contract::TokenBands {
            input_fresh: input,
            cache_read: 0,
            cache_write_5m: 0,
            cache_write_1h: 0,
            cache_write_unspecified: 0,
            output_total: output,
            reasoning: None,
            unclassified: 0,
        })
    }

    async fn insert(db: &crate::Database, at: &str, model: &str, adapter: &str, u: TokenUsage) {
        upsert_usage(
            db.writer(),
            &UsageRecord {
                adapter_id: adapter.to_owned(),
                session_id: "s".to_owned(),
                dedup_key: format!("{at}-{model}-{}", uuid::Uuid::new_v4()),
                model_id: Some(model.to_owned()),
                occurred_at: chrono::DateTime::parse_from_rfc3339(at)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
                measurement_source: "provider_exact".to_owned(),
                request_kind: "turn".to_owned(),
                usage: u,
                is_sidechain: false,
                agent_id: None,
                agent_type: None,
                raw_json: None,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_day_is_reported_per_model_rather_than_blended() {
        // The property the whole module shape exists for. Two models on one
        // day: costing the day's summed tokens at either single rate gives the
        // wrong answer, and only per-model slices make the right one reachable.
        let db = db().await;
        insert(
            &db,
            "2026-08-13T09:00:00Z",
            "claude-opus-5",
            "claude_code",
            usage(1_000_000, 0),
        )
        .await;
        insert(
            &db,
            "2026-08-13T10:00:00Z",
            "gpt-5.6-terra",
            "codex",
            usage(1_000_000, 0),
        )
        .await;

        let slices = by_day_model(db.reader(), &Filter::all()).await.unwrap();
        assert_eq!(slices.len(), 2, "one row per model, not one per day");
        assert!(slices.iter().all(|s| s.at == "2026-08-13"));

        let models: Vec<_> = slices
            .iter()
            .filter_map(|s| s.model_id.as_deref())
            .collect();
        assert!(models.contains(&"claude-opus-5"));
        assert!(models.contains(&"gpt-5.6-terra"));

        // Folded for the chart, the day is one row again — but the fold is the
        // caller's choice, made after costing rather than before it.
        let folded = fold_by_bucket(&slices);
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].1.input_fresh, 2_000_000);
    }

    #[tokio::test]
    async fn a_range_excludes_what_falls_outside_it() {
        let db = db().await;
        insert(
            &db,
            "2026-08-11T12:00:00Z",
            "m",
            "claude_code",
            usage(10, 1),
        )
        .await;
        insert(
            &db,
            "2026-08-13T12:00:00Z",
            "m",
            "claude_code",
            usage(20, 2),
        )
        .await;
        insert(
            &db,
            "2026-08-15T12:00:00Z",
            "m",
            "claude_code",
            usage(40, 4),
        )
        .await;

        let f = Filter::all()
            .since("2026-08-12T00:00:00.000Z")
            .until("2026-08-14T00:00:00.000Z");
        let t = totals(db.reader(), &f).await.unwrap();
        assert_eq!(t.requests, 1);
        assert_eq!(t.input_fresh, 20);
    }

    #[tokio::test]
    async fn the_range_is_half_open_so_adjacent_days_do_not_double_count() {
        // `until` is exclusive. A closed range would count midnight twice when
        // two queries are put side by side, which is how a weekly total ends up
        // slightly larger than the sum of its days.
        let db = db().await;
        insert(&db, "2026-08-13T00:00:00Z", "m", "claude_code", usage(5, 0)).await;

        let before = Filter::all().until("2026-08-13T00:00:00.000Z");
        let after = Filter::all().since("2026-08-13T00:00:00.000Z");
        assert_eq!(totals(db.reader(), &before).await.unwrap().requests, 0);
        assert_eq!(totals(db.reader(), &after).await.unwrap().requests, 1);
    }

    #[tokio::test]
    async fn an_adapter_filter_selects_one_agent() {
        let db = db().await;
        insert(
            &db,
            "2026-08-13T09:00:00Z",
            "claude-opus-5",
            "claude_code",
            usage(100, 0),
        )
        .await;
        insert(
            &db,
            "2026-08-13T10:00:00Z",
            "gpt-5.6-terra",
            "codex",
            usage(200, 0),
        )
        .await;

        let t = totals(db.reader(), &Filter::all().adapter("codex"))
            .await
            .unwrap();
        assert_eq!(t.requests, 1);
        assert_eq!(t.input_fresh, 200);
    }

    #[tokio::test]
    async fn hours_bucket_separately_from_days() {
        let db = db().await;
        insert(&db, "2026-08-13T09:30:00Z", "m", "claude_code", usage(1, 0)).await;
        insert(&db, "2026-08-13T09:45:00Z", "m", "claude_code", usage(2, 0)).await;
        insert(&db, "2026-08-13T11:00:00Z", "m", "claude_code", usage(4, 0)).await;

        let hours = fold_by_bucket(&by_hour_model(db.reader(), &Filter::all()).await.unwrap());
        assert_eq!(hours.len(), 2);
        assert_eq!(hours[0].0, "2026-08-13T09");
        assert_eq!(hours[0].1.input_fresh, 3);
        assert_eq!(hours[1].1.input_fresh, 4);

        let days = fold_by_bucket(&by_day_model(db.reader(), &Filter::all()).await.unwrap());
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].1.input_fresh, 7);
    }

    #[tokio::test]
    async fn a_failed_request_is_left_out_of_the_totals_and_counted_apart() {
        let db = db().await;
        insert(
            &db,
            "2026-08-13T09:00:00Z",
            "m",
            "claude_code",
            usage(100, 10),
        )
        .await;
        crate::repo::record_failure(
            db.writer(),
            "claude_code",
            "s",
            "boom",
            chrono::DateTime::parse_from_rfc3339("2026-08-13T09:30:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            "the call failed",
        )
        .await
        .unwrap();

        let t = totals(db.reader(), &Filter::all()).await.unwrap();
        assert_eq!(t.requests, 1, "the failure is not a success");
        assert_eq!(t.grand_total(), 110);
        assert_eq!(failed_count(db.reader(), &Filter::all()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn reasoning_stays_absent_when_nobody_reported_it() {
        // Summing across buckets must not turn "not reported" into zero.
        let db = db().await;
        insert(
            &db,
            "2026-08-13T09:00:00Z",
            "m",
            "claude_code",
            usage(10, 1),
        )
        .await;
        insert(
            &db,
            "2026-08-14T09:00:00Z",
            "m",
            "claude_code",
            usage(10, 1),
        )
        .await;

        let slices = by_day_model(db.reader(), &Filter::all()).await.unwrap();
        assert!(slices.iter().all(|s| s.totals.reasoning.is_none()));
        assert_eq!(fold(slices.iter().map(|s| &s.totals)).reasoning, None);
    }

    #[tokio::test]
    async fn folding_a_reported_bucket_with_an_unreported_one_keeps_the_number() {
        let mut reported = Totals {
            reasoning: Some(35),
            reasoning_reported_by: 1,
            requests: 1,
            ..Totals::default()
        };
        let silent = Totals {
            reasoning: None,
            requests: 1,
            ..Totals::default()
        };
        reported.add(&silent);
        assert_eq!(reported.reasoning, Some(35));
        assert_eq!(
            reported.reasoning_reported_by, 1,
            "still only one reported it"
        );
        assert_eq!(reported.requests, 2);
    }

    #[tokio::test]
    async fn an_empty_database_totals_to_nothing_rather_than_failing() {
        let db = db().await;
        let t = totals(db.reader(), &Filter::all()).await.unwrap();
        assert_eq!(t.requests, 0);
        assert_eq!(t.reasoning, None);
        assert!(
            by_day_model(db.reader(), &Filter::all())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn models_and_adapters_come_back_busiest_first() {
        let db = db().await;
        for _ in 0..3 {
            insert(
                &db,
                "2026-08-13T09:00:00Z",
                "busy",
                "claude_code",
                usage(1, 0),
            )
            .await;
        }
        insert(&db, "2026-08-13T09:00:00Z", "quiet", "codex", usage(1, 0)).await;

        let models = by_model(db.reader(), &Filter::all()).await.unwrap();
        assert_eq!(models[0].0.as_deref(), Some("busy"));
        assert_eq!(models[0].1.requests, 3);

        let adapters = by_adapter(db.reader(), &Filter::all()).await.unwrap();
        assert_eq!(adapters[0].0, "claude_code");
    }
}
