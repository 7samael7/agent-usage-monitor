//! Claude Desktop's daily counter, kept because it does not keep itself.
//!
//! Separate from `repo` on purpose. Everything there feeds per-task and
//! per-model aggregates; nothing here may, because this number has no model, no
//! input/output split and no request boundary. Keeping the queries apart is the
//! cheap version of making that mistake hard.

use sqlx::{Row, Sqlite, pool::Pool};

use crate::now_sql;
use crate::repo::Result;

/// One day's total, as sampled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyRow {
    pub day: String,
    pub tokens: i64,
    pub last_seen_at: String,
}

/// Record a sample of the counter.
///
/// The stored value is the maximum seen for that day. Within a day the counter
/// only grows, so this is idempotent — sampling every two seconds records the
/// same number as sampling once — and it protects the boundary: a reading taken
/// moments after midnight is a fresh small number for a *new* day, and must
/// never be able to reduce the day that just finished.
pub async fn record_daily(pool: &Pool<Sqlite>, day: &str, tokens: u64) -> Result<()> {
    let now = now_sql();
    sqlx::query(
        "INSERT INTO desktop_daily_tokens (day, tokens, source, first_seen_at, last_seen_at)
         VALUES (?1, ?2, 'claude_desktop', ?3, ?3)
         ON CONFLICT(day) DO UPDATE SET
           tokens       = MAX(tokens, excluded.tokens),
           last_seen_at = excluded.last_seen_at",
    )
    .bind(day)
    .bind(i64::try_from(tokens).unwrap_or(i64::MAX))
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Recent days, newest first.
pub async fn daily_history(pool: &Pool<Sqlite>, limit: i64) -> Result<Vec<DailyRow>> {
    let rows = sqlx::query(
        "SELECT day, tokens, last_seen_at
           FROM desktop_daily_tokens
          ORDER BY day DESC
          LIMIT ?1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|r| {
            Ok(DailyRow {
                day: r.try_get("day")?,
                tokens: r.try_get("tokens")?,
                last_seen_at: r.try_get("last_seen_at")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    async fn db() -> crate::Database {
        crate::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn a_repeated_sample_does_not_inflate_the_day() {
        // The engine samples every pass. If that accumulated, an idle machine
        // would report millions of tokens by lunchtime.
        let db = db().await;
        for _ in 0..50 {
            record_daily(db.writer(), "2026-08-13", 1_119_656)
                .await
                .unwrap();
        }
        let history = daily_history(db.reader(), 30).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].tokens, 1_119_656);
    }

    #[tokio::test]
    async fn a_growing_counter_is_followed_upwards() {
        let db = db().await;
        record_daily(db.writer(), "2026-08-13", 100).await.unwrap();
        record_daily(db.writer(), "2026-08-13", 900).await.unwrap();
        assert_eq!(daily_history(db.reader(), 30).await.unwrap()[0].tokens, 900);
    }

    #[tokio::test]
    async fn a_lower_reading_never_lowers_a_recorded_day() {
        let db = db().await;
        record_daily(db.writer(), "2026-08-13", 900).await.unwrap();
        record_daily(db.writer(), "2026-08-13", 3).await.unwrap();
        assert_eq!(daily_history(db.reader(), 30).await.unwrap()[0].tokens, 900);
    }

    #[tokio::test]
    async fn days_come_back_newest_first() {
        let db = db().await;
        for (day, n) in [("2026-08-11", 1), ("2026-08-13", 3), ("2026-08-12", 2)] {
            record_daily(db.writer(), day, n).await.unwrap();
        }
        let history = daily_history(db.reader(), 30).await.unwrap();
        assert_eq!(
            history.iter().map(|r| r.day.as_str()).collect::<Vec<_>>(),
            vec!["2026-08-13", "2026-08-12", "2026-08-11"]
        );
    }

    #[tokio::test]
    async fn this_counter_never_reaches_the_request_aggregates() {
        // The structural guarantee: nothing recorded here can appear in a
        // per-task or per-model total, because it is not in ai_request at all.
        let db = db().await;
        record_daily(db.writer(), "2026-08-13", 1_119_656)
            .await
            .unwrap();

        let (requests,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ai_request")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(requests, 0);
    }
}
