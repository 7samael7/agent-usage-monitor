//! Persistence for prices and exchange rates.
//!
//! Both are stored in **nano-units** — integers — because the whole point of
//! this application's money handling is that a rate never passes through a
//! float. `$15.00 per million tokens` is `15_000_000_000` here, and the caller
//! converts to and from `Decimal` at the boundary.
//!
//! This module deliberately does not know what a `Decimal` is, and does not
//! depend on `aum-pricing`. Storage holds integers and strings; the pricing
//! crate owns the arithmetic. That keeps the dependency edge pointing one way
//! and means a replacement backend can reproduce the table from the schema
//! alone.
//!
//! Nothing here updates a row. A corrected price is a new version with a later
//! `effective_from`; the old one stays exactly where it was, because a
//! benchmark run in March must still show March's numbers in August.

use sqlx::{Row, Sqlite, pool::Pool};

use crate::repo::Result;
use crate::{now_sql, to_sql_time};

/// Nano-units per whole unit. `$1.00` is `1_000_000_000`.
pub const NANO: i64 = 1_000_000_000;

/// One stored version of one model's price, in nano-USD per million tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceVersionRow {
    pub id: String,
    pub model_id: String,
    pub input_per_mtok: i64,
    pub output_per_mtok: i64,
    pub cache_read_per_mtok: Option<i64>,
    pub cache_write_5m_per_mtok: Option<i64>,
    pub cache_write_1h_per_mtok: Option<i64>,
    pub effective_from: String,
    /// `seed`, `user` or `updater`.
    pub source: String,
    pub note: Option<String>,
}

/// One stored exchange rate, in nano-units of the quote currency per 1 USD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FxRateRow {
    pub id: String,
    pub quote_currency: String,
    pub rate_nano: i64,
    pub as_of: String,
    pub source: String,
}

/// Register a model so a price can reference it.
///
/// Models show up in real transcripts long before any price list mentions
/// them, so `first_seen_at` records when this machine saw one — independently
/// of whether it can be costed.
pub async fn ensure_model(pool: &Pool<Sqlite>, model_id: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO model (id, first_seen_at) VALUES (?1, ?2)
         ON CONFLICT(id) DO NOTHING",
    )
    .bind(model_id)
    .bind(now_sql())
    .execute(pool)
    .await?;
    Ok(())
}

/// Every price version ever recorded, oldest first.
///
/// All of them, not just the current one: the history is shown in the interface
/// so that a changed price is visible as a change rather than as a number that
/// silently differs from last week's.
pub async fn list_price_versions(pool: &Pool<Sqlite>) -> Result<Vec<PriceVersionRow>> {
    let rows = sqlx::query(
        "SELECT id, model_id, input_per_mtok, output_per_mtok, cache_read_per_mtok,
                cache_write_5m_per_mtok, cache_write_1h_per_mtok, effective_from, source, note
           FROM pricing_version
          ORDER BY model_id, effective_from",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| PriceVersionRow {
            id: r.get("id"),
            model_id: r.get("model_id"),
            input_per_mtok: r.get("input_per_mtok"),
            output_per_mtok: r.get("output_per_mtok"),
            cache_read_per_mtok: r.get("cache_read_per_mtok"),
            cache_write_5m_per_mtok: r.get("cache_write_5m_per_mtok"),
            cache_write_1h_per_mtok: r.get("cache_write_1h_per_mtok"),
            effective_from: r.get("effective_from"),
            source: r.get("source"),
            note: r.get("note"),
        })
        .collect())
}

/// Add a price version.
///
/// Append-only, and the previous version for the same model is closed off by
/// `effective_until` so the two do not both claim to be current. Closing is the
/// one write that touches an existing row, and it changes no rate — it records
/// when that rate stopped applying.
pub async fn insert_price_version(pool: &Pool<Sqlite>, row: &PriceVersionRow) -> Result<()> {
    ensure_model(pool, &row.model_id).await?;

    sqlx::query(
        "UPDATE pricing_version
            SET effective_until = ?2
          WHERE model_id = ?1 AND effective_until IS NULL AND effective_from <= ?2",
    )
    .bind(&row.model_id)
    .bind(&row.effective_from)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO pricing_version
           (id, model_id, input_per_mtok, output_per_mtok, cache_read_per_mtok,
            cache_write_5m_per_mtok, cache_write_1h_per_mtok,
            effective_from, effective_until, source, note, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11)",
    )
    .bind(&row.id)
    .bind(&row.model_id)
    .bind(row.input_per_mtok)
    .bind(row.output_per_mtok)
    .bind(row.cache_read_per_mtok)
    .bind(row.cache_write_5m_per_mtok)
    .bind(row.cache_write_1h_per_mtok)
    .bind(&row.effective_from)
    .bind(&row.source)
    .bind(&row.note)
    .bind(now_sql())
    .execute(pool)
    .await?;

    Ok(())
}

/// The most recent exchange rate for each quote currency.
pub async fn latest_fx_rates(pool: &Pool<Sqlite>) -> Result<Vec<FxRateRow>> {
    let rows = sqlx::query(
        "SELECT id, quote_currency, rate_nano, as_of, source
           FROM exchange_rate
          WHERE (quote_currency, as_of) IN (
                SELECT quote_currency, MAX(as_of) FROM exchange_rate GROUP BY quote_currency)
          ORDER BY quote_currency",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| FxRateRow {
            id: r.get("id"),
            quote_currency: r.get("quote_currency"),
            rate_nano: r.get("rate_nano"),
            as_of: r.get("as_of"),
            source: r.get("source"),
        })
        .collect())
}

/// Record an exchange rate.
pub async fn insert_fx_rate(pool: &Pool<Sqlite>, row: &FxRateRow) -> Result<()> {
    sqlx::query(
        "INSERT INTO exchange_rate (id, quote_currency, rate_nano, as_of, source, fetched_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(&row.id)
    .bind(&row.quote_currency)
    .bind(row.rate_nano)
    .bind(&row.as_of)
    .bind(&row.source)
    .bind(to_sql_time(chrono::Utc::now()))
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    async fn db() -> crate::Database {
        let dir = tempfile::tempdir().unwrap();
        let d = crate::open(dir.path()).await.unwrap();
        std::mem::forget(dir);
        d
    }

    fn price(id: &str, model: &str, input: i64, from: &str, source: &str) -> PriceVersionRow {
        PriceVersionRow {
            id: id.to_owned(),
            model_id: model.to_owned(),
            input_per_mtok: input,
            output_per_mtok: 75 * NANO,
            cache_read_per_mtok: Some(NANO + NANO / 2),
            cache_write_5m_per_mtok: Some(18 * NANO),
            cache_write_1h_per_mtok: Some(30 * NANO),
            effective_from: from.to_owned(),
            source: source.to_owned(),
            note: None,
        }
    }

    #[tokio::test]
    async fn a_price_survives_a_round_trip_exactly() {
        let db = db().await;
        let row = price(
            "v1",
            "claude-opus-5",
            15 * NANO,
            "2026-08-01T00:00:00.000Z",
            "user",
        );
        insert_price_version(db.writer(), &row).await.unwrap();

        let back = list_price_versions(db.reader()).await.unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0], row);
    }

    #[tokio::test]
    async fn a_correction_adds_a_version_and_keeps_the_old_one() {
        // The property the whole append-only design exists for: a run costed in
        // March must still show March's numbers after an August correction.
        let db = db().await;
        insert_price_version(
            db.writer(),
            &price("v1", "m", 10 * NANO, "2026-03-01T00:00:00.000Z", "user"),
        )
        .await
        .unwrap();
        insert_price_version(
            db.writer(),
            &price("v2", "m", 15 * NANO, "2026-08-01T00:00:00.000Z", "user"),
        )
        .await
        .unwrap();

        let all = list_price_versions(db.reader()).await.unwrap();
        assert_eq!(all.len(), 2, "the earlier version must still exist");
        assert_eq!(all[0].input_per_mtok, 10 * NANO);
        assert_eq!(all[1].input_per_mtok, 15 * NANO);
    }

    #[tokio::test]
    async fn superseding_a_price_closes_the_previous_one() {
        // Two rows both claiming to be current would make "the price" depend on
        // which one a query happened to see first.
        let db = db().await;
        insert_price_version(
            db.writer(),
            &price("v1", "m", 10 * NANO, "2026-03-01T00:00:00.000Z", "user"),
        )
        .await
        .unwrap();
        insert_price_version(
            db.writer(),
            &price("v2", "m", 15 * NANO, "2026-08-01T00:00:00.000Z", "user"),
        )
        .await
        .unwrap();

        let open: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pricing_version WHERE effective_until IS NULL",
        )
        .fetch_one(db.reader())
        .await
        .unwrap();
        assert_eq!(open, 1);
    }

    #[tokio::test]
    async fn pricing_a_model_registers_it() {
        // pricing_version references model(id); a price for a model nobody has
        // recorded yet must still be accepted, because that is exactly the case
        // this application exists to handle.
        let db = db().await;
        insert_price_version(
            db.writer(),
            &price(
                "v1",
                "gpt-5.6-sol",
                2 * NANO,
                "2026-08-01T00:00:00.000Z",
                "user",
            ),
        )
        .await
        .unwrap();

        let seen: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model WHERE id = 'gpt-5.6-sol'")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(seen, 1);
    }

    #[tokio::test]
    async fn only_the_newest_rate_per_currency_comes_back() {
        let db = db().await;
        for (id, cur, nano, as_of) in [
            ("a", "EUR", 920_000_000, "2026-08-01T00:00:00.000Z"),
            ("b", "EUR", 930_000_000, "2026-08-10T00:00:00.000Z"),
            ("c", "CZK", 22_500_000_000, "2026-08-05T00:00:00.000Z"),
        ] {
            insert_fx_rate(
                db.writer(),
                &FxRateRow {
                    id: id.to_owned(),
                    quote_currency: cur.to_owned(),
                    rate_nano: nano,
                    as_of: as_of.to_owned(),
                    source: "manual".to_owned(),
                },
            )
            .await
            .unwrap();
        }

        let latest = latest_fx_rates(db.reader()).await.unwrap();
        assert_eq!(latest.len(), 2);
        let eur = latest.iter().find(|r| r.quote_currency == "EUR").unwrap();
        assert_eq!(eur.rate_nano, 930_000_000, "the newer EUR rate should win");
    }
}
