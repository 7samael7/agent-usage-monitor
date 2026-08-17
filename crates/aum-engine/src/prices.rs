//! Loading and recording prices.
//!
//! The seed table is compiled in and holds only rates that were published. The
//! database holds what the user typed. Both are merged at startup and after
//! every edit, and a user entry wins a tie, because someone who has gone to a
//! provider's pricing page and typed in a figure knows something a shipped
//! default does not.
//!
//! Every model this machine actually runs — `claude-opus-5`, `claude-fable-5`,
//! `gpt-5.6-sol` — is newer than any public price list. Without this path those
//! models are permanently uncosted; with it, the honest "no price for this
//! model" becomes something the user can answer rather than only be told.

use aum_db::Database;
use aum_db::pricing::{FxRateRow, PriceVersionRow};
use aum_pricing::{ExchangeRate, ModelPricing, PriceTable, Rates, nano};
use rust_decimal::Decimal;

/// Everything needed to turn tokens into a displayable amount.
///
/// Held by reference so a request pays nothing to build one, and carries the
/// currency alongside the rates so that no code path can convert an amount
/// without also carrying what it was converted with.
#[derive(Debug, Clone, Copy)]
pub struct CostContext<'a> {
    pub table: &'a PriceTable,
    pub fx: &'a [ExchangeRate],
    pub currency: aum_contract::Currency,
}

impl<'a> CostContext<'a> {
    /// A context that does no conversion, for callers that only want USD.
    #[must_use]
    pub const fn usd(table: &'a PriceTable) -> Self {
        Self {
            table,
            fx: &[],
            currency: aum_contract::Currency::Usd,
        }
    }

    #[must_use]
    pub fn rate(&self) -> Option<&ExchangeRate> {
        self.fx.iter().find(|r| r.quote == self.currency)
    }

    /// Convert a USD amount into the display currency.
    ///
    /// Returns a [`aum_contract::Measured`], so the certainty lost in
    /// converting cannot be dropped between here and the screen: a converted
    /// figure is at best calculated, and one converted with a week-old rate is
    /// an estimate that says so.
    #[must_use]
    pub fn present(
        &self,
        amount: aum_contract::Measured<aum_contract::Money>,
    ) -> aum_contract::Measured<aum_contract::Money> {
        aum_pricing::convert(amount, self.currency, self.rate(), chrono::Utc::now())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PriceError {
    #[error(transparent)]
    Db(#[from] aum_db::DbError),
    /// A figure that cannot be represented at nano precision. In practice a
    /// typo — refused rather than wrapped into a plausible small number.
    #[error("{field} is not a storable rate: {value}")]
    Unstorable { field: &'static str, value: String },
}

fn to_nano(field: &'static str, value: Decimal) -> Result<i64, PriceError> {
    nano::from_decimal(value).ok_or_else(|| PriceError::Unstorable {
        field,
        value: value.to_string(),
    })
}

fn to_rates(row: &PriceVersionRow) -> Rates {
    // A missing cache rate means the provider charges cache at the input rate,
    // which is the documented default for both providers. It does not mean
    // free — costing a cache read at zero understates a long Claude session by
    // most of its total.
    let input = nano::to_decimal(row.input_per_mtok);
    Rates {
        input_per_mtok: input,
        output_per_mtok: nano::to_decimal(row.output_per_mtok),
        cache_read_per_mtok: row.cache_read_per_mtok.map_or(input, nano::to_decimal),
        cache_write_5m_per_mtok: row.cache_write_5m_per_mtok.map_or(input, nano::to_decimal),
        cache_write_1h_per_mtok: row.cache_write_1h_per_mtok.map_or(input, nano::to_decimal),
    }
}

/// The seed table plus everything recorded in this database.
pub async fn load_table(db: &Database) -> Result<PriceTable, aum_db::DbError> {
    let mut table = PriceTable::seed();
    for row in aum_db::pricing::list_price_versions(db.reader()).await? {
        table.push(ModelPricing {
            version_id: row.id.clone(),
            model_id: row.model_id.clone(),
            rates: to_rates(&row),
            effective_from: row.effective_from.clone(),
            source: row.source.clone(),
            note: row.note.clone(),
        });
    }
    Ok(table)
}

/// Record a price the user has entered, and return it as it will now be used.
///
/// `effective_from` is now, so the new figure applies from this moment and
/// every already-costed request keeps the version it was costed with.
pub async fn save_price(
    db: &Database,
    model_id: &str,
    rates: &Rates,
    note: Option<String>,
) -> Result<ModelPricing, PriceError> {
    let effective_from = aum_db::to_sql_time(chrono::Utc::now());
    let row = PriceVersionRow {
        id: uuid::Uuid::new_v4().to_string(),
        model_id: model_id.to_owned(),
        input_per_mtok: to_nano("input", rates.input_per_mtok)?,
        output_per_mtok: to_nano("output", rates.output_per_mtok)?,
        cache_read_per_mtok: Some(to_nano("cache read", rates.cache_read_per_mtok)?),
        cache_write_5m_per_mtok: Some(to_nano(
            "5-minute cache write",
            rates.cache_write_5m_per_mtok,
        )?),
        cache_write_1h_per_mtok: Some(to_nano(
            "1-hour cache write",
            rates.cache_write_1h_per_mtok,
        )?),
        effective_from: effective_from.clone(),
        source: "user".to_owned(),
        note,
    };

    aum_db::pricing::insert_price_version(db.writer(), &row).await?;

    Ok(ModelPricing {
        version_id: row.id,
        model_id: row.model_id,
        rates: rates.clone(),
        effective_from,
        source: row.source,
        note: row.note,
    })
}

/// The newest rate recorded for each currency.
pub async fn load_fx(db: &Database) -> Result<Vec<ExchangeRate>, aum_db::DbError> {
    let rows = aum_db::pricing::latest_fx_rates(db.reader()).await?;
    Ok(rows.iter().filter_map(row_to_rate).collect())
}

fn row_to_rate(row: &FxRateRow) -> Option<ExchangeRate> {
    Some(ExchangeRate {
        quote: parse_currency(&row.quote_currency)?,
        rate: nano::to_decimal(row.rate_nano),
        as_of: chrono::DateTime::parse_from_rfc3339(&row.as_of)
            .ok()?
            .with_timezone(&chrono::Utc),
        source: row.source.clone(),
    })
}

#[must_use]
pub fn parse_currency(code: &str) -> Option<aum_contract::Currency> {
    match code.to_ascii_uppercase().as_str() {
        "USD" => Some(aum_contract::Currency::Usd),
        "EUR" => Some(aum_contract::Currency::Eur),
        "CZK" => Some(aum_contract::Currency::Czk),
        _ => None,
    }
}

/// Record an exchange rate the user has entered.
///
/// Rates are typed in, never fetched. Fetching one would mean this process
/// making an outbound request, and the privacy claim in Settings is worth more
/// than the convenience — particularly for a number that changes a fraction of
/// a percent a day and is applied to an already-approximate cost.
pub async fn save_fx(
    db: &Database,
    quote: aum_contract::Currency,
    rate: Decimal,
) -> Result<ExchangeRate, PriceError> {
    let as_of = chrono::Utc::now();
    let row = FxRateRow {
        id: uuid::Uuid::new_v4().to_string(),
        quote_currency: quote.code().to_owned(),
        rate_nano: to_nano("rate", rate)?,
        as_of: aum_db::to_sql_time(as_of),
        source: "manual".to_owned(),
    };
    aum_db::pricing::insert_fx_rate(db.writer(), &row).await?;
    Ok(ExchangeRate {
        quote,
        rate,
        as_of,
        source: row.source,
    })
}

/// Everything the process knows about what things cost.
///
/// Both halves are append-only in storage; this is the in-memory projection,
/// rebuilt after an edit rather than mutated in place, so a half-applied change
/// can never be observed. Held behind a lock because a price can be entered
/// while the application is running and the next figure must use it.
pub struct MoneyState {
    pub table: PriceTable,
    /// The newest rate per currency. Entered by the user — nothing here makes
    /// an outbound request to find one.
    pub fx: Vec<ExchangeRate>,
}

impl MoneyState {
    /// Load both halves from storage.
    ///
    /// A failure to load prices is not fatal: an application that shows tokens
    /// without costs is far more useful than one that refuses to start over a
    /// price list.
    pub async fn load(db: &Database) -> Self {
        let table = load_table(db).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not load stored prices; using the seed table");
            PriceTable::seed()
        });
        let fx = load_fx(db).await.unwrap_or_default();
        Self { table, fx }
    }

    #[must_use]
    pub fn cost_context(&self, currency: aum_contract::Currency) -> CostContext<'_> {
        CostContext {
            table: &self.table,
            fx: &self.fx,
            currency,
        }
    }
}

// ── Costing aggregated usage ────────────────────────────────────────────────

/// Rebuild a `TokenUsage` from stored columns.
///
/// The columns were written from a `TokenUsage`, so the buckets are already
/// disjoint and no provider semantics are re-applied here.
fn to_usage(t: &aum_db::usage::Totals) -> aum_domain::TokenUsage {
    let n = |v: i64| u64::try_from(v).unwrap_or(0);
    aum_domain::TokenUsage::from_bands(aum_contract::TokenBands {
        input_fresh: n(t.input_fresh),
        cache_read: n(t.cache_read),
        cache_write_5m: n(t.cache_write_5m),
        cache_write_1h: n(t.cache_write_1h),
        cache_write_unspecified: n(t.cache_write_unspecified),
        output_total: n(t.output_total),
        reasoning: t.reasoning.map(n),
        unclassified: n(t.unclassified),
    })
}

/// Cost a set of per-model slices.
///
/// **Per model, then summed.** Rates differ between models by up to ten times,
/// so pricing a bucket's combined tokens at any single rate produces a figure
/// that matches no actual rate — and looks entirely plausible while doing it.
///
/// A slice whose model has no price does not silently contribute zero. The
/// underlying [`aum_pricing::cost_of_many`] returns a `Partial` carrying both
/// the priced portion and how many models were missing, so the figure reads as
/// a floor rather than a total. With nothing priced at all it is `Unavailable`,
/// and with nothing measured at all it is `Unavailable` too — an empty day has
/// no cost to report, which is not the same claim as a day that was free.
#[must_use]
pub fn cost_of_slices<'a>(
    slices: impl IntoIterator<Item = (&'a Option<String>, &'a aum_db::usage::Totals)>,
    table: &PriceTable,
) -> aum_contract::Measured<aum_contract::Money> {
    let items: Vec<_> = slices
        .into_iter()
        .map(|(model, totals)| (to_usage(totals), model.clone()))
        .collect();
    aum_pricing::cost_of_many(&items, table)
}

/// Cost each bucket of a day/hour series, keeping the bucket labels.
#[must_use]
pub fn cost_by_bucket(
    slices: &[aum_db::usage::Slice],
    table: &PriceTable,
) -> Vec<(String, aum_contract::Measured<aum_contract::Money>)> {
    let mut out: Vec<(String, Vec<&aum_db::usage::Slice>)> = Vec::new();
    for s in slices {
        match out.last_mut() {
            Some((at, group)) if *at == s.at => group.push(s),
            _ => out.push((s.at.clone(), vec![s])),
        }
    }
    out.into_iter()
        .map(|(at, group)| {
            let cost = cost_of_slices(group.iter().map(|s| (&s.model_id, &s.totals)), table);
            (at, cost)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::str::FromStr as _;

    async fn db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        let d = aum_db::open(dir.path()).await.unwrap();
        std::mem::forget(dir);
        d
    }

    fn rates(input: &str, output: &str) -> Rates {
        let d = |s: &str| Decimal::from_str(s).unwrap();
        Rates {
            input_per_mtok: d(input),
            output_per_mtok: d(output),
            cache_read_per_mtok: d("1.50"),
            cache_write_5m_per_mtok: d("18.75"),
            cache_write_1h_per_mtok: d("30.00"),
        }
    }

    #[tokio::test]
    async fn a_price_the_user_enters_becomes_usable_immediately() {
        // A model the seed has never heard of — which is the case that matters,
        // because a new one appears every few weeks and is unpriceable until
        // somebody types a rate in. Deliberately not a real model id: this is
        // about the mechanism, and naming a shipped one would make the test
        // fail the day that model gets seeded.
        const UNSEEDED: &str = "not-a-real-model-2027";

        let db = db().await;
        assert!(!load_table(&db).await.unwrap().has_price(UNSEEDED));

        save_price(&db, UNSEEDED, &rates("15.00", "75.00"), None)
            .await
            .unwrap();

        let table = load_table(&db).await.unwrap();
        let found = table.lookup(UNSEEDED).unwrap();
        assert_eq!(found.rates.input_per_mtok, Decimal::from_str("15").unwrap());
        assert_eq!(found.source, "user");
    }

    #[tokio::test]
    async fn a_fresh_machine_can_price_the_models_these_agents_run() {
        // The bug this covers: the rates lived only in the database of the
        // machine they were typed on, so the same binary on a second machine
        // showed "not priced" for everything. The seed ships in the binary;
        // the database does not travel.
        let db = db().await;
        let table = load_table(&db).await.unwrap();
        for model in [
            "claude-opus-5",
            "claude-fable-5",
            "claude-sonnet-5",
            "gpt-5.6-terra",
            "gpt-5.5",
        ] {
            assert!(
                table.has_price(model),
                "{model} is unpriced on a machine with an empty database"
            );
        }
    }

    #[tokio::test]
    async fn rates_survive_storage_at_full_decimal_precision() {
        // A float round trip is exactly what this must not do.
        let db = db().await;
        save_price(&db, "m", &rates("0.075", "1.234567891"), None)
            .await
            .unwrap();

        let table = load_table(&db).await.unwrap();
        let r = &table.lookup("m").unwrap().rates;
        assert_eq!(r.input_per_mtok, Decimal::from_str("0.075").unwrap());
        assert_eq!(r.output_per_mtok, Decimal::from_str("1.234567891").unwrap());
    }

    #[tokio::test]
    async fn a_user_price_overrides_a_seeded_one_for_the_same_model() {
        let db = db().await;
        let seeded = PriceTable::seed();
        let Some(known) = seeded.models().first().map(|m| m.model_id.clone()) else {
            return;
        };

        save_price(&db, &known, &rates("999.00", "999.00"), None)
            .await
            .unwrap();

        let table = load_table(&db).await.unwrap();
        let found = table.lookup(&known).unwrap();
        assert_eq!(found.source, "user");
        assert_eq!(
            found.rates.input_per_mtok,
            Decimal::from_str("999").unwrap()
        );
    }

    #[tokio::test]
    async fn an_absurd_rate_is_refused_rather_than_stored_wrong() {
        let db = db().await;
        let err = save_price(&db, "m", &rates("100000000000", "1.00"), None)
            .await
            .unwrap_err();
        assert!(matches!(err, PriceError::Unstorable { field: "input", .. }));

        // And nothing was written, so a rejected edit leaves no trace.
        assert!(load_table(&db).await.unwrap().lookup("m").is_none());
    }

    #[tokio::test]
    async fn an_exchange_rate_round_trips_and_is_dated() {
        let db = db().await;
        assert!(load_fx(&db).await.unwrap().is_empty());

        save_fx(
            &db,
            aum_contract::Currency::Eur,
            Decimal::from_str("0.92").unwrap(),
        )
        .await
        .unwrap();

        let rates = load_fx(&db).await.unwrap();
        assert_eq!(rates.len(), 1);
        assert_eq!(rates[0].rate, Decimal::from_str("0.92").unwrap());
        assert_eq!(rates[0].source, "manual");
        // Just written, so it cannot be stale.
        assert!(!rates[0].is_stale(chrono::Utc::now()));
    }

    #[test]
    fn currency_codes_parse_case_insensitively_and_reject_the_rest() {
        assert_eq!(parse_currency("eur"), Some(aum_contract::Currency::Eur));
        assert_eq!(parse_currency("CZK"), Some(aum_contract::Currency::Czk));
        assert_eq!(parse_currency("GBP"), None);
    }

    #[tokio::test]
    async fn a_missing_cache_rate_falls_back_to_input_rather_than_to_zero() {
        // Costing cache reads at zero would understate a long Claude session by
        // most of its total, and would look entirely plausible.
        let db = db().await;
        aum_db::pricing::insert_price_version(
            db.writer(),
            &PriceVersionRow {
                id: "v1".to_owned(),
                model_id: "m".to_owned(),
                input_per_mtok: 3 * aum_db::pricing::NANO,
                output_per_mtok: 15 * aum_db::pricing::NANO,
                cache_read_per_mtok: None,
                cache_write_5m_per_mtok: None,
                cache_write_1h_per_mtok: None,
                effective_from: "2026-01-01T00:00:00.000Z".to_owned(),
                source: "user".to_owned(),
                note: None,
            },
        )
        .await
        .unwrap();

        let table = load_table(&db).await.unwrap();
        let r = &table.lookup("m").unwrap().rates;
        assert_eq!(r.cache_read_per_mtok, Decimal::from(3));
        assert_eq!(r.cache_write_1h_per_mtok, Decimal::from(3));
    }
}
