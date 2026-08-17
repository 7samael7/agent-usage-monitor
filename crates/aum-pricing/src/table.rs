//! The price table.
//!
//! Append-only. A price is never edited in place: correcting one creates a new
//! version with a new `effective_from`, and the previous version stays. That is
//! what lets a benchmark run in March still display March's numbers in August,
//! and makes recalculating an explicit, audited action rather than a side
//! effect of editing a row.

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

    /// The seed table.
    ///
    /// Deliberately small. Every model this machine actually runs —
    /// `claude-opus-5`, `claude-fable-5`, `gpt-5.6-sol`, `gpt-5.6-terra` — is
    /// absent from every public price list, and inventing a plausible number
    /// for them would be worse than saying so: an invented rate produces a
    /// confident total that is wrong by an unknown factor, where an absent one
    /// produces "unavailable" and a prompt to enter the real figure.
    #[must_use]
    pub fn seed() -> Self {
        Self::from_entries(
            serde_json::from_str::<Vec<ModelPricing>>(include_str!("../seed/models.json"))
                .unwrap_or_default(),
        )
    }

    /// The price to use for a model.
    ///
    /// Exact match only. Prefix or family matching would quietly price
    /// `claude-opus-5` at `claude-opus-4`'s rate, which is precisely the
    /// substitution this application must not make.
    #[must_use]
    pub fn lookup(&self, model_id: &str) -> Option<&ModelPricing> {
        self.entries
            .iter()
            .filter(|e| e.model_id == model_id)
            .max_by(|a, b| {
                // A user's own entry beats a seeded one; otherwise the most
                // recently effective wins.
                let user = (a.source == "user").cmp(&(b.source == "user"));
                user.then_with(|| a.effective_from.cmp(&b.effective_from))
            })
    }

    #[must_use]
    pub fn models(&self) -> Vec<&ModelPricing> {
        self.entries.iter().collect()
    }

    #[must_use]
    pub fn has_price(&self, model_id: &str) -> bool {
        self.lookup(model_id).is_some()
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

    #[test]
    fn the_most_recent_version_wins() {
        let table = PriceTable::from_entries(vec![
            entry("v1", "m", "10.00", "2026-01-01T00:00:00.000Z", "seed"),
            entry("v2", "m", "15.00", "2026-06-01T00:00:00.000Z", "seed"),
        ]);
        assert_eq!(table.lookup("m").unwrap().version_id, "v2");
    }

    #[test]
    fn a_user_entry_beats_a_seeded_one() {
        // Someone who typed in a rate knows something the seed does not.
        let table = PriceTable::from_entries(vec![
            entry("v2", "m", "15.00", "2026-06-01T00:00:00.000Z", "seed"),
            entry("u1", "m", "12.00", "2026-01-01T00:00:00.000Z", "user"),
        ]);
        assert_eq!(table.lookup("m").unwrap().version_id, "u1");
    }

    #[test]
    fn older_versions_are_not_destroyed_by_a_newer_one() {
        // A benchmark run in March must still show March's numbers.
        let mut table = PriceTable::from_entries(vec![entry(
            "v1",
            "m",
            "10.00",
            "2026-01-01T00:00:00.000Z",
            "seed",
        )]);
        table.push(entry(
            "v2",
            "m",
            "15.00",
            "2026-06-01T00:00:00.000Z",
            "seed",
        ));
        assert_eq!(table.len(), 2);
        assert!(table.models().iter().any(|e| e.version_id == "v1"));
    }

    #[test]
    fn lookup_is_exact_and_never_falls_back_to_a_similar_model() {
        // The substitution this application must not make: pricing
        // claude-opus-5 at claude-opus-4's rate would produce a confident total
        // wrong by an unknown factor.
        let table = PriceTable::from_entries(vec![entry(
            "v1",
            "claude-opus-4-8",
            "10.00",
            "2026-01-01T00:00:00.000Z",
            "seed",
        )]);
        assert!(table.lookup("claude-opus-5").is_none());
        assert!(table.lookup("claude-opus").is_none());
        assert!(table.lookup("claude-opus-4-8").is_some());
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
    fn the_seed_holds_one_price_per_model() {
        // Two entries for the same model would both be "current", and which one
        // won would depend on the order they happened to be written in.
        let seed = PriceTable::seed();
        let mut ids: Vec<&str> = seed.models().iter().map(|e| e.model_id.as_str()).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(before, ids.len(), "a model is seeded twice");
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
