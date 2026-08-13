//! The one number Claude Desktop does report.
//!
//! Claude Desktop keeps two quantitative files. `plan-usage-history.json` holds
//! plan-limit percentages — `{"u":{"fh":0,"sd":13}}`, five-hour and seven-day
//! windows — from which no token count can be derived by any defensible means.
//! `buddy-tokens.json` holds this:
//!
//! ```json
//! { "tokens-today": { "date": "2026-08-13", "tokens": 1119656 } }
//! ```
//!
//! A real integer count. It is worth reading, and it is worth being precise
//! about what it is not:
//!
//! * **One running daily total.** No model, no input/output split, no cache
//!   breakdown, no per-request rows.
//! * **Unattributable.** No session id reaches the file, so it can never belong
//!   to a task.
//! * **Unpriceable.** Without a model and an input/output split there is no
//!   honest cost, and inventing one would be exactly the confident-but-wrong
//!   number this application exists to refuse.
//! * **Kept for today only.** The counter resets when the date changes, so
//!   whatever it read yesterday is gone unless something sampled it. That is
//!   the one thing a continuously-running monitor can add: the history the
//!   application itself does not keep.
//!
//! It is therefore surfaced as its own clearly-scoped figure on the
//! Applications screen, and never folded into a task, a session or a cost.

use std::path::{Path, PathBuf};

/// What the counter said when we last looked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyTokens {
    /// Local calendar day, as the application writes it (`YYYY-MM-DD`).
    pub day: String,
    pub tokens: u64,
}

/// Where Claude Desktop keeps it.
#[must_use]
pub fn counter_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("Claude")
        .join("buddy-tokens.json")
}

/// Read the counter, if it is there and says something usable.
///
/// Every failure is `None` rather than an error: this file belongs to another
/// application, which is free to change or remove it, and a monitor that cannot
/// read an optional extra should carry on measuring everything else.
#[must_use]
pub fn read(home: &Path) -> Option<DailyTokens> {
    let raw = std::fs::read_to_string(counter_path(home)).ok()?;
    parse(&raw)
}

fn parse(raw: &str) -> Option<DailyTokens> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let today = value.get("tokens-today")?;
    let day = today.get("date")?.as_str()?;
    let tokens = today.get("tokens")?.as_u64()?;

    // A blank date is the application's own "not yet initialised" state, and a
    // count without a day it belongs to cannot be recorded against anything.
    if day.is_empty() {
        return None;
    }

    Some(DailyTokens {
        day: day.to_owned(),
        tokens,
    })
}

/// Sample the counter and record it, returning what was read.
///
/// Stored per day with a component-wise maximum, because within a day the
/// counter only ever grows — so re-reading it is free, and a sample taken just
/// before midnight is not overwritten by the fresh zero taken just after.
pub async fn sample(
    db: &aum_db::Database,
    home: &Path,
) -> Result<Option<DailyTokens>, aum_db::DbError> {
    let Some(reading) = read(home) else {
        return Ok(None);
    };
    aum_db::desktop::record_daily(db.writer(), &reading.day, reading.tokens).await?;
    Ok(Some(reading))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn write(home: &Path, contents: &str) {
        let dir = counter_path(home);
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::write(dir, contents).unwrap();
    }

    #[test]
    fn reads_the_real_shape() {
        // Exactly as observed on this machine.
        let got = parse(r#"{"tokens-today":{"date":"2026-08-13","tokens":1119656}}"#).unwrap();
        assert_eq!(got.day, "2026-08-13");
        assert_eq!(got.tokens, 1_119_656);
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(read(home.path()), None);
    }

    #[test]
    fn a_shape_we_do_not_recognise_is_declined_rather_than_guessed() {
        // The file belongs to another application and may change without
        // notice. Reading something unexpected must not produce a number.
        for raw in [
            "{}",
            r#"{"tokens-today":{}}"#,
            r#"{"tokens-today":{"date":"2026-08-13"}}"#,
            r#"{"tokens-today":{"tokens":5}}"#,
            r#"{"tokens-today":{"date":"2026-08-13","tokens":"lots"}}"#,
            r#"{"tokens-today":{"date":"","tokens":5}}"#,
            "not json at all",
        ] {
            assert_eq!(parse(raw), None, "should have declined: {raw}");
        }
    }

    #[test]
    fn a_zero_count_is_a_real_reading() {
        // Zero tokens today is a measurement. It is not the same as absent, and
        // the distinction is the whole point of returning Option here.
        let got = parse(r#"{"tokens-today":{"date":"2026-08-13","tokens":0}}"#).unwrap();
        assert_eq!(got.tokens, 0);
    }

    #[tokio::test]
    async fn sampling_records_what_it_read() {
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            r#"{"tokens-today":{"date":"2026-08-13","tokens":1119656}}"#,
        );
        let db = aum_db::open_in_memory().await.unwrap();

        let got = sample(&db, home.path()).await.unwrap().unwrap();
        assert_eq!(got.tokens, 1_119_656);

        let history = aum_db::desktop::daily_history(db.reader(), 30)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].tokens, 1_119_656);
    }

    #[tokio::test]
    async fn a_days_total_never_goes_backwards() {
        // The counter resets at midnight. A sample taken just after must not
        // overwrite the previous day's final figure with a fresh zero, and a
        // stale re-read must not lower today's.
        let home = tempfile::tempdir().unwrap();
        let db = aum_db::open_in_memory().await.unwrap();

        write(
            home.path(),
            r#"{"tokens-today":{"date":"2026-08-13","tokens":900}}"#,
        );
        sample(&db, home.path()).await.unwrap();
        write(
            home.path(),
            r#"{"tokens-today":{"date":"2026-08-13","tokens":12}}"#,
        );
        sample(&db, home.path()).await.unwrap();

        let history = aum_db::desktop::daily_history(db.reader(), 30)
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].tokens, 900);
    }

    #[tokio::test]
    async fn a_new_day_is_a_new_row_rather_than_an_increase() {
        let home = tempfile::tempdir().unwrap();
        let db = aum_db::open_in_memory().await.unwrap();

        write(
            home.path(),
            r#"{"tokens-today":{"date":"2026-08-12","tokens":500}}"#,
        );
        sample(&db, home.path()).await.unwrap();
        write(
            home.path(),
            r#"{"tokens-today":{"date":"2026-08-13","tokens":10}}"#,
        );
        sample(&db, home.path()).await.unwrap();

        let history = aum_db::desktop::daily_history(db.reader(), 30)
            .await
            .unwrap();
        assert_eq!(history.len(), 2);
        // Newest first.
        assert_eq!(history[0].day, "2026-08-13");
        assert_eq!(history[0].tokens, 10);
        assert_eq!(history[1].day, "2026-08-12");
        assert_eq!(history[1].tokens, 500);
    }
}
