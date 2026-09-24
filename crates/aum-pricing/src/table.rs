//! The price table.
//!
//! Append-only. A price is never edited in place: a new figure is a new version
//! with its own `effective_from`, and the previous version stays. Usage is
//! priced at the version in force when it happened, so a request made in March
//! keeps March's rate after an August change — not because anything about it
//! was stored, but because nothing about it changed.

use std::cmp::Ordering;
use std::collections::HashMap;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Rates for one model, in USD per million tokens.
///
/// Cache writes are split by time-to-live because the multipliers genuinely
/// differ — roughly 1.25x input for five minutes against 2x for an hour — and
/// real Claude Code sessions are dominated by the hour tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rates {
    #[serde(with = "rust_decimal::serde::str")]
    pub input_per_mtok: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub output_per_mtok: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub cache_read_per_mtok: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub cache_write_5m_per_mtok: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub cache_write_1h_per_mtok: Decimal,
}

/// One version of one model's price.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub version_id: String,
    pub model_id: String,
    pub rates: Rates,
    /// When this version takes over: an RFC 3339 UTC timestamp in the fixed
    /// width `aum_db::to_sql_time` writes, so that it compares as text with the
    /// moment a request happened.
    pub effective_from: String,
    /// `seed`, `user` or `updater`. A user's own entry wins ties, because
    /// someone who has typed in a rate knows something we do not.
    pub source: String,
    /// Where the figure came from. Published rates change, and a number with no
    /// provenance cannot be checked against the page it was copied from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Prices, looked up by model.
#[derive(Debug, Clone, Default)]
pub struct PriceTable {
    entries: Vec<ModelPricing>,
}

impl PriceTable {
    #[must_use]
    pub fn from_entries(entries: Vec<ModelPricing>) -> Self {
        Self { entries }
    }

    /// The seed table: the rates shipped in the binary.
    ///
    /// Only rates a provider has published, each carrying the page it was read
    /// from and the day it was read. A model no page prices is left out rather
    /// than given a plausible number: an invented rate produces a confident
    /// total that is wrong by an unknown factor, where an absent one produces
    /// "unavailable" and a prompt to enter the real figure. A model whose price
    /// has changed keeps every version, so its older usage stays at the rate it
    /// was charged.
    #[must_use]
    pub fn seed() -> Self {
        Self::from_entries(
            serde_json::from_str::<Vec<ModelPricing>>(include_str!("../seed/models.json"))
                .unwrap_or_default(),
        )
    }

    /// The price in force for a model at an instant.
    ///
    /// Every version of a model, shipped or entered, sits on one timeline. A
    /// version applies from its `effective_from` until the next one begins,
    /// and a user's entry beats a shipped one only when both begin at the same
    /// instant: a shipped version that begins later records a price change the
    /// entry could not have known about.
    ///
    /// Usage older than every version takes the earliest. That is what lets a
    /// price entered today cover a model's past: the past was unpriced rather
    /// than priced at something else, so there is no earlier rate to keep.
    ///
    /// Exact match only. Prefix or family matching would quietly price
    /// `claude-opus-5` at `claude-opus-4`'s rate, which is precisely the
    /// substitution this application must not make.
    #[must_use]
    pub fn lookup_at(&self, model_id: &str, at: &str) -> Option<&ModelPricing> {
        let in_force = |at: &str| {
            self.entries
                .iter()
                .filter(|e| e.model_id == model_id && e.effective_from.as_str() <= at)
                .max_by(|a, b| timeline(a, b))
        };
        in_force(at).or_else(|| {
            let first = self
                .entries
                .iter()
                .filter(|e| e.model_id == model_id)
                .map(|e| e.effective_from.as_str())
                .min()?;
            in_force(first)
        })
    }

    /// Every instant at which some model's price changes, oldest first.
    ///
    /// Usage is summed before it is priced, and one sum is priced at one rate,
    /// so no sum may straddle one of these. A model's first version is not a
    /// change — it covers everything before it as well — so its start is left
    /// out.
    #[must_use]
    pub fn breaks(&self) -> Vec<String> {
        let mut first: HashMap<&str, &str> = HashMap::new();
        for e in &self.entries {
            let start = first.entry(&e.model_id).or_insert(&e.effective_from);
            if e.effective_from.as_str() < *start {
                *start = &e.effective_from;
            }
        }
        let mut out: Vec<String> = self
            .entries
            .iter()
            .filter(|e| first.get(e.model_id.as_str()) != Some(&e.effective_from.as_str()))
            .map(|e| e.effective_from.clone())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    #[must_use]
    pub fn models(&self) -> Vec<&ModelPricing> {
        self.entries.iter().collect()
    }

    /// Whether any version prices this model, at any time.
    #[must_use]
    pub fn has_price(&self, model_id: &str) -> bool {
        self.entries.iter().any(|e| e.model_id == model_id)
    }

    /// Add a version. Never replaces an existing one.
    pub fn push(&mut self, pricing: ModelPricing) {
        self.entries.push(pricing);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Order along a model's timeline: by start, and at the same start a user's
/// entry after a shipped one, so that the entry is the one left in force.
fn timeline(a: &ModelPricing, b: &ModelPricing) -> Ordering {
    let user = |p: &ModelPricing| p.source == "user";
    a.effective_from
        .cmp(&b.effective_from)
        .then_with(|| user(a).cmp(&user(b)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::str::FromStr as _;

    fn rates(input: &str) -> Rates {
        let d = |s: &str| Decimal::from_str(s).unwrap();
        Rates {
            input_per_mtok: d(input),
            output_per_mtok: d("75.00"),
            cache_read_per_mtok: d("1.50"),
            cache_write_5m_per_mtok: d("18.75"),
            cache_write_1h_per_mtok: d("30.00"),
        }
    }

    fn entry(version: &str, model: &str, input: &str, from: &str, source: &str) -> ModelPricing {
        ModelPricing {
            version_id: version.to_owned(),
            model_id: model.to_owned(),
            rates: rates(input),
            effective_from: from.to_owned(),
            source: source.to_owned(),
            note: None,
        }
    }

    const MARCH: &str = "2026-03-01T00:00:00.000Z";
    const AUGUST: &str = "2026-08-01T00:00:00.000Z";

    fn version(table: &PriceTable, model: &str, at: &str) -> String {
        table
            .lookup_at(model, at)
            .map(|p| p.version_id.clone())
            .unwrap_or_default()
    }

    #[test]
    fn a_march_request_keeps_marchs_rate_after_an_august_change() {
        // The property the append-only table exists for. The August price is
        // the current one, and it is not the one a March request was charged.
        let table = PriceTable::from_entries(vec![
            entry("march", "m", "10.00", MARCH, "seed"),
            entry("august", "m", "15.00", AUGUST, "seed"),
        ]);
        assert_eq!(version(&table, "m", "2026-03-15T09:00:00.000Z"), "march");
        assert_eq!(version(&table, "m", "2026-07-31T23:59:59.999Z"), "march");
        assert_eq!(
            version(&table, "m", AUGUST),
            "august",
            "a version applies from its first instant"
        );
        assert_eq!(version(&table, "m", "2026-09-24T12:00:00.000Z"), "august");
    }

    #[test]
    fn usage_older_than_every_version_takes_the_earliest() {
        let table = PriceTable::from_entries(vec![
            entry("march", "m", "10.00", MARCH, "seed"),
            entry("august", "m", "15.00", AUGUST, "seed"),
        ]);
        assert_eq!(version(&table, "m", "2025-12-31T00:00:00.000Z"), "march");
    }

    #[test]
    fn a_price_entered_for_an_unpriced_model_covers_its_past() {
        // The case a naive date check gets wrong: the price is typed in today
        // for a model that has been running for weeks with none. Those weeks
        // were unpriced, not priced at something else, so there is no earlier
        // rate for them to keep.
        let table = PriceTable::from_entries(vec![entry(
            "typed",
            "m",
            "10.00",
            "2026-09-24T14:05:00.000Z",
            "user",
        )]);
        assert_eq!(version(&table, "m", "2026-09-08T05:21:12.639Z"), "typed");
    }

    #[test]
    fn a_user_entry_takes_over_from_when_it_was_made() {
        // `aum price` corrects a stale seed from the moment it is run. What
        // happened before keeps the rate that was in force then.
        let table = PriceTable::from_entries(vec![
            entry("shipped", "m", "5.00", MARCH, "seed"),
            entry("typed", "m", "4.00", "2026-09-01T12:00:00.000Z", "user"),
        ]);
        assert_eq!(version(&table, "m", "2026-08-31T12:00:00.000Z"), "shipped");
        assert_eq!(version(&table, "m", "2026-09-02T12:00:00.000Z"), "typed");
    }

    #[test]
    fn a_user_entry_wins_a_tie_with_a_shipped_version() {
        // Both begin at the same instant, and someone who typed a rate in knows
        // something the seed does not. The order they were loaded in must not
        // decide it.
        for entries in [
            vec![
                entry("shipped", "m", "5.00", MARCH, "seed"),
                entry("typed", "m", "4.00", MARCH, "user"),
            ],
            vec![
                entry("typed", "m", "4.00", MARCH, "user"),
                entry("shipped", "m", "5.00", MARCH, "seed"),
            ],
        ] {
            let table = PriceTable::from_entries(entries);
            assert_eq!(version(&table, "m", "2026-04-01T00:00:00.000Z"), "typed");
            assert_eq!(
                version(&table, "m", "2026-01-01T00:00:00.000Z"),
                "typed",
                "and as the earliest version, too"
            );
        }
    }

    #[test]
    fn a_shipped_version_that_begins_later_takes_over_from_a_user_entry() {
        // A price typed in on 13 August corrected the seed as it stood that
        // day. A release that ships the cut of 21 August knows something the
        // entry could not, so it takes over from the 21st, and the entry still
        // prices the days in between.
        let table = PriceTable::from_entries(vec![
            entry(
                "typed",
                "gpt-5.6-sol",
                "5.00",
                "2026-08-13T11:51:48.188Z",
                "user",
            ),
            entry(
                "cut",
                "gpt-5.6-sol",
                "4.00",
                "2026-08-21T00:00:00.000Z",
                "seed",
            ),
        ]);
        assert_eq!(
            version(&table, "gpt-5.6-sol", "2026-08-20T12:00:00.000Z"),
            "typed"
        );
        assert_eq!(
            version(&table, "gpt-5.6-sol", "2026-08-21T12:00:00.000Z"),
            "cut"
        );
    }

    #[test]
    fn older_versions_are_not_destroyed_by_a_newer_one() {
        // A benchmark run in March must still show March's numbers.
        let mut table = PriceTable::from_entries(vec![entry("v1", "m", "10.00", MARCH, "seed")]);
        table.push(entry("v2", "m", "15.00", AUGUST, "seed"));
        assert_eq!(table.len(), 2);
        assert_eq!(version(&table, "m", "2026-03-15T00:00:00.000Z"), "v1");
    }

    #[test]
    fn lookup_is_exact_and_never_falls_back_to_a_similar_model() {
        // The substitution this application must not make: pricing
        // claude-opus-5 at claude-opus-4's rate would produce a confident total
        // wrong by an unknown factor.
        let table =
            PriceTable::from_entries(vec![entry("v1", "claude-opus-4-8", "10.00", MARCH, "seed")]);
        let at = "2026-09-24T00:00:00.000Z";
        assert!(table.lookup_at("claude-opus-5", at).is_none());
        assert!(table.lookup_at("claude-opus", at).is_none());
        assert!(table.lookup_at("claude-opus-4-8", at).is_some());
        assert!(!table.has_price("claude-opus-5"));
    }

    #[test]
    fn breaks_are_the_price_changes_and_not_the_first_prices() {
        // A model's first version covers everything before it, so its start
        // changes nothing, and splitting usage there would only add rows.
        let table = PriceTable::from_entries(vec![
            entry("a1", "a", "10.00", MARCH, "seed"),
            entry("a2", "a", "15.00", AUGUST, "seed"),
            entry("b1", "b", "1.00", "2026-05-01T00:00:00.000Z", "seed"),
            entry("b2", "b", "2.00", AUGUST, "user"),
        ]);
        assert_eq!(table.breaks(), vec![AUGUST.to_owned()]);
    }

    #[test]
    fn the_seed_table_parses() {
        let seed = PriceTable::seed();
        assert!(!seed.is_empty(), "the seed file should contain something");
    }

    #[test]
    fn every_seeded_rate_says_where_it_came_from() {
        // The seed is the one place this application states a number nobody on
        // this machine typed in. An earlier version of this test asserted the
        // opposite — that the seed must *not* price the models in use here,
        // because at the time their rates were not published anywhere. They are
        // now, so the rule that replaces it is the one that was always the
        // point: a seeded price carries the page it was read from and the day
        // it was read, or it does not belong in the file.
        for entry in PriceTable::seed().models() {
            let note = entry.note.as_deref().unwrap_or_default();
            assert!(
                note.contains("claude.com") || note.contains("openai.com"),
                "{}: a seeded price needs its source, got {note:?}",
                entry.model_id
            );
            assert!(
                note.contains("checked"),
                "{}: and the date it was checked, got {note:?}",
                entry.model_id
            );
        }
    }

    #[test]
    fn no_seeded_rate_is_zero_or_upside_down() {
        // Two copy-paste failures that produce confident, wrong totals: a cache
        // read priced at zero (understates a long session by most of its cost)
        // and input/output transposed (understates every heavy generation).
        for entry in PriceTable::seed().models() {
            let r = &entry.rates;
            for (what, rate) in [
                ("input", r.input_per_mtok),
                ("output", r.output_per_mtok),
                ("cache read", r.cache_read_per_mtok),
                ("5m cache write", r.cache_write_5m_per_mtok),
                ("1h cache write", r.cache_write_1h_per_mtok),
            ] {
                assert!(
                    rate > Decimal::ZERO,
                    "{}: {what} priced at {rate}",
                    entry.model_id
                );
            }
            assert!(
                r.output_per_mtok > r.input_per_mtok,
                "{}: output ({}) should cost more than input ({}) — transposed?",
                entry.model_id,
                r.output_per_mtok,
                r.input_per_mtok
            );
            assert!(
                r.cache_read_per_mtok < r.input_per_mtok,
                "{}: a cache read should be cheaper than fresh input",
                entry.model_id
            );
        }
    }

    #[test]
    fn no_two_seeded_versions_of_a_model_begin_together() {
        // Two versions beginning at the same instant would both be in force,
        // and which one won would depend on the order they were written in.
        let seed = PriceTable::seed();
        let mut starts: Vec<(&str, &str)> = seed
            .models()
            .iter()
            .map(|e| (e.model_id.as_str(), e.effective_from.as_str()))
            .collect();
        let before = starts.len();
        starts.sort_unstable();
        starts.dedup();
        assert_eq!(
            before,
            starts.len(),
            "two versions of one model begin together"
        );
    }

    #[test]
    fn every_seeded_version_id_is_distinct() {
        let seed = PriceTable::seed();
        let mut ids: Vec<&str> = seed
            .models()
            .iter()
            .map(|e| e.version_id.as_str())
            .collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(before, ids.len(), "a version id is used twice");
    }

    #[test]
    fn every_seeded_start_is_written_the_way_request_times_are() {
        // A start is compared with request times as text, which is exact only
        // while both share one fixed width. Written with an offset, or without
        // the milliseconds, it would sort on the wrong side of requests made
        // close to it.
        for entry in PriceTable::seed().models() {
            let parsed = chrono::DateTime::parse_from_rfc3339(&entry.effective_from)
                .unwrap_or_else(|e| panic!("{}: {e}", entry.version_id));
            let stored = parsed
                .with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string();
            assert_eq!(entry.effective_from, stored, "{}", entry.version_id);
        }
    }

    #[test]
    fn a_seeded_price_cut_leaves_the_usage_before_it_alone() {
        // The two cuts this machine lived through. Terra's July usage was
        // charged the launch price, and Sol's promotion began on 21 August.
        // Pricing either model's earlier requests at today's rate would
        // understate them by a fifth or more.
        let seed = PriceTable::seed();
        let input = |model: &str, at: &str| {
            seed.lookup_at(model, at)
                .map(|p| p.rates.input_per_mtok.to_string())
        };
        assert_eq!(
            input("gpt-5.6-terra", "2026-07-20T12:00:00.000Z").as_deref(),
            Some("2.50")
        );
        assert_eq!(
            input("gpt-5.6-terra", "2026-07-31T12:00:00.000Z").as_deref(),
            Some("2.00")
        );
        assert_eq!(
            input("gpt-5.6-sol", "2026-08-13T12:25:27.678Z").as_deref(),
            Some("5.00")
        );
        assert_eq!(
            input("gpt-5.6-sol", "2026-09-24T12:00:00.000Z").as_deref(),
            Some("4.00")
        );
    }

    #[test]
    fn rates_survive_a_json_round_trip_as_decimals() {
        // Money is never a float, including in the file it is loaded from.
        let original = entry("v1", "m", "15.00", "2026-01-01T00:00:00.000Z", "seed");
        let json = serde_json::to_string(&original).unwrap();
        assert!(json.contains("\"15.00\""), "rates must be strings: {json}");
        let back: ModelPricing = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }
}
