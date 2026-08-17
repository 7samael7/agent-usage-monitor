//! Ordering a table.
//!
//! Shared by the interface and the subcommands so both agree on what "sorted by
//! cost" means — in particular on the one question a naive comparator gets
//! wrong: **a row whose cost is unavailable is not a cheap row.** It sorts to
//! the end in both directions rather than to whichever end zero would land on.

use std::cmp::Ordering;
use std::collections::HashMap;

use aum_contract::{Measured, Money};
use aum_db::usage::Totals;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// The first column: a date, an hour, or a model id.
    Label,
    Requests,
    Tokens,
    Cost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: Key,
    pub descending: bool,
}

/// What the first column holds, which is all that separates "newest first" from
/// "Z to A".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Labels {
    Time,
    Name,
}

impl Sort {
    /// Today at the top.
    ///
    /// The default for the daily and hourly tables: the row you opened the tab
    /// to read is the current one, and it should not be at the bottom of three
    /// hundred days of history.
    pub const NEWEST_FIRST: Self = Self {
        key: Key::Label,
        descending: true,
    };

    /// Today at the bottom.
    ///
    /// The default for printed output, where the rows scroll and the last one
    /// is what ends up beside your prompt.
    pub const OLDEST_FIRST: Self = Self {
        key: Key::Label,
        descending: false,
    };

    /// The default where the first column is a name rather than a time, and
    /// "most of it" is the interesting order.
    pub const LARGEST_FIRST: Self = Self {
        key: Key::Tokens,
        descending: true,
    };

    /// What pressing a sort key does.
    ///
    /// Pressing the column already sorted flips it. Any other column starts in
    /// the direction it is usually read: biggest first for a quantity, A to Z
    /// for a name. The daily table's *initial* order is set explicitly rather
    /// than by this rule, because for dates "usually read" is newest first.
    #[must_use]
    pub fn press(self, key: Key) -> Self {
        if self.key == key {
            return Self {
                key,
                descending: !self.descending,
            };
        }
        Self {
            key,
            descending: key != Key::Label,
        }
    }

    /// The order in words, for the table's title.
    ///
    /// Spelled out rather than left to an arrow alone: an arrow says which way,
    /// a sentence says what it means.
    #[must_use]
    pub fn describe(self, labels: Labels) -> &'static str {
        match (self.key, self.descending, labels) {
            (Key::Label, true, Labels::Time) => "newest first",
            (Key::Label, false, Labels::Time) => "oldest first",
            (Key::Label, true, Labels::Name) => "by name, Z to A",
            (Key::Label, false, Labels::Name) => "by name, A to Z",
            (Key::Requests, true, _) => "most requests first",
            (Key::Requests, false, _) => "fewest requests first",
            (Key::Tokens, true, _) => "most tokens first",
            (Key::Tokens, false, _) => "fewest tokens first",
            (Key::Cost, true, _) => "dearest first, unpriced last",
            (Key::Cost, false, _) => "cheapest first, unpriced last",
        }
    }

    /// The marker for a column header, or nothing if this is not that column.
    #[must_use]
    pub fn marker(self, key: Key) -> &'static str {
        if self.key != key {
            return "";
        }
        if self.descending { " ▼" } else { " ▲" }
    }
}

/// One row of a sortable table.
pub struct Row<'a> {
    pub label: &'a str,
    pub totals: &'a Totals,
    /// `None` where no cost was computed for this row at all, which renders the
    /// same as an unavailable one — never as zero.
    pub cost: Option<&'a Measured<Money>>,
}

/// Pair each bucket with its cost.
///
/// The two vectors are built from the same slices and arrive in the same order,
/// but they are joined by label rather than by position: a mis-paired cost would
/// attach one day's money to another day's tokens, and nothing on screen would
/// look wrong.
#[must_use]
pub fn join<'a>(
    series: &'a [(String, Totals)],
    costs: &'a [(String, Measured<Money>)],
) -> Vec<Row<'a>> {
    let by_label: HashMap<&str, &Measured<Money>> =
        costs.iter().map(|(at, m)| (at.as_str(), m)).collect();
    series
        .iter()
        .map(|(label, totals)| Row {
            label,
            totals,
            cost: by_label.get(label.as_str()).copied(),
        })
        .collect()
}

/// Pair each model with the cost at its own position.
#[must_use]
pub fn join_models<'a>(
    models: &'a [(Option<String>, Totals)],
    costs: &'a [Measured<Money>],
    unnamed: &'a str,
) -> Vec<Row<'a>> {
    models
        .iter()
        .enumerate()
        .map(|(index, (model, totals))| Row {
            label: model.as_deref().unwrap_or(unnamed),
            totals,
            cost: costs.get(index),
        })
        .collect()
}

/// Reorder in place. Stable, so rows that compare equal keep the order they
/// arrived in.
pub fn apply(rows: &mut [Row<'_>], sort: Sort) {
    rows.sort_by(|a, b| compare(a, b, sort));
}

fn compare(a: &Row<'_>, b: &Row<'_>, sort: Sort) -> Ordering {
    // Dates and hours are `YYYY-MM-DD` and `YYYY-MM-DDTHH`, where lexical order
    // is chronological order. That is a property of the format, so it is worth
    // saying out loud: the day column is sorted as text and is still a timeline.
    let ord = match sort.key {
        Key::Label => a.label.cmp(b.label),
        Key::Requests => a.totals.requests.cmp(&b.totals.requests),
        Key::Tokens => a.totals.grand_total().cmp(&b.totals.grand_total()),
        Key::Cost => return compare_cost(a, b, sort.descending),
    };
    if sort.descending { ord.reverse() } else { ord }
}

/// Cost, with the unmeasured rows pushed to the end whichever way it is sorted.
///
/// Treating an unavailable cost as zero would put every unpriced model at the
/// top of "cheapest first" — a list of the things this tool knows least about,
/// presented as the things that cost the least.
fn compare_cost(a: &Row<'_>, b: &Row<'_>, descending: bool) -> Ordering {
    match (a.cost.and_then(|m| m.value), b.cost.and_then(|m| m.value)) {
        (Some(x), Some(y)) => {
            if descending {
                y.cmp(&x)
            } else {
                x.cmp(&y)
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use aum_contract::MeasurementSource;
    use rust_decimal::Decimal;

    fn totals(requests: i64, tokens: i64) -> Totals {
        Totals {
            requests,
            output_total: tokens,
            ..Totals::default()
        }
    }

    fn money(v: &str) -> Measured<Money> {
        Measured::calculated(
            Money::new(v.parse::<Decimal>().unwrap()),
            MeasurementSource::ProviderReported,
        )
    }

    fn unpriced() -> Measured<Money> {
        Measured::unavailable(aum_contract::UnavailableReason::NoPricingForModel {
            model_id: "m".to_owned(),
        })
    }

    struct Fixture {
        series: Vec<(String, Totals)>,
        costs: Vec<(String, Measured<Money>)>,
    }

    fn fixture() -> Fixture {
        Fixture {
            series: vec![
                ("2026-08-11".to_owned(), totals(10, 500)),
                ("2026-08-12".to_owned(), totals(30, 100)),
                ("2026-08-17".to_owned(), totals(20, 300)),
            ],
            costs: vec![
                ("2026-08-11".to_owned(), money("5.00")),
                ("2026-08-12".to_owned(), unpriced()),
                ("2026-08-17".to_owned(), money("1.00")),
            ],
        }
    }

    fn order(sort: Sort) -> Vec<String> {
        let f = fixture();
        let mut rows = join(&f.series, &f.costs);
        apply(&mut rows, sort);
        rows.iter().map(|r| r.label.to_owned()).collect()
    }

    #[test]
    fn the_default_puts_today_at_the_top() {
        // The whole point of the default: the current day is the row you came
        // to read, and it must not be at the bottom of a year of history.
        assert_eq!(
            order(Sort::NEWEST_FIRST),
            ["2026-08-17", "2026-08-12", "2026-08-11"]
        );
    }

    #[test]
    fn dates_sort_chronologically_even_though_they_are_text() {
        let asc = Sort {
            key: Key::Label,
            descending: false,
        };
        assert_eq!(order(asc), ["2026-08-11", "2026-08-12", "2026-08-17"]);
    }

    #[test]
    fn hours_within_a_day_stay_in_order() {
        let series = vec![
            ("2026-08-17T09".to_owned(), totals(1, 1)),
            ("2026-08-17T10".to_owned(), totals(1, 1)),
            ("2026-08-16T23".to_owned(), totals(1, 1)),
        ];
        let mut rows = join(&series, &[]);
        apply(&mut rows, Sort::NEWEST_FIRST);
        let labels: Vec<&str> = rows.iter().map(|r| r.label).collect();
        assert_eq!(labels, ["2026-08-17T10", "2026-08-17T09", "2026-08-16T23"]);
    }

    #[test]
    fn sorting_by_tokens_and_by_requests_are_different_orders() {
        // If they were the same the fixture would prove nothing.
        let tokens = order(Sort {
            key: Key::Tokens,
            descending: true,
        });
        let requests = order(Sort {
            key: Key::Requests,
            descending: true,
        });
        assert_eq!(tokens, ["2026-08-11", "2026-08-17", "2026-08-12"]);
        assert_eq!(requests, ["2026-08-12", "2026-08-17", "2026-08-11"]);
    }

    #[test]
    fn an_unpriced_row_sorts_last_whichever_way_cost_is_sorted() {
        // The failure this prevents: unavailable compared as zero, so every
        // model the tool cannot price heads the "cheapest first" list — the
        // things it knows least about, presented as the things that cost least.
        let dearest = order(Sort {
            key: Key::Cost,
            descending: true,
        });
        let cheapest = order(Sort {
            key: Key::Cost,
            descending: false,
        });
        assert_eq!(dearest, ["2026-08-11", "2026-08-17", "2026-08-12"]);
        assert_eq!(cheapest, ["2026-08-17", "2026-08-11", "2026-08-12"]);
        assert_eq!(dearest.last(), cheapest.last(), "unpriced stays at the end");
    }

    #[test]
    fn a_missing_cost_row_is_treated_as_unavailable_not_as_zero() {
        // Costs joined by label: a bucket with no cost entry at all must behave
        // like an unpriced one, not sort as free.
        let f = fixture();
        let mut rows = join(&f.series, &f.costs[..1]);
        apply(
            &mut rows,
            Sort {
                key: Key::Cost,
                descending: false,
            },
        );
        assert_eq!(rows[0].label, "2026-08-11");
        assert!(rows[1].cost.is_none() && rows[2].cost.is_none());
    }

    #[test]
    fn costs_are_joined_by_label_not_by_position() {
        // A cost list in a different order must still attach to the right day.
        let f = fixture();
        let reversed: Vec<(String, Measured<Money>)> = f.costs.iter().rev().cloned().collect();
        let rows = join(&f.series, &reversed);
        let for_17 = rows.iter().find(|r| r.label == "2026-08-17").unwrap();
        assert_eq!(
            for_17.cost.and_then(|m| m.value),
            money("1.00").value,
            "the 17th's cost followed the 17th"
        );
    }

    #[test]
    fn pressing_the_sorted_column_flips_it_and_any_other_column_switches() {
        let s = Sort::NEWEST_FIRST;
        assert!(!s.press(Key::Label).descending, "same column flips");
        assert!(s.press(Key::Label).press(Key::Label).descending, "and back");

        let by_cost = s.press(Key::Cost);
        assert_eq!(by_cost.key, Key::Cost);
        assert!(by_cost.descending, "a quantity starts with the biggest");

        assert!(
            !Sort::LARGEST_FIRST.press(Key::Label).descending,
            "a name starts at A"
        );
    }

    #[test]
    fn every_order_can_say_what_it_is() {
        for key in [Key::Label, Key::Requests, Key::Tokens, Key::Cost] {
            for descending in [true, false] {
                for labels in [Labels::Time, Labels::Name] {
                    let s = Sort { key, descending };
                    assert!(!s.describe(labels).is_empty());
                }
            }
        }
        assert_eq!(Sort::NEWEST_FIRST.describe(Labels::Time), "newest first");
        assert_eq!(Sort::NEWEST_FIRST.describe(Labels::Name), "by name, Z to A");
    }

    #[test]
    fn only_the_sorted_column_is_marked() {
        let s = Sort::NEWEST_FIRST;
        assert_eq!(s.marker(Key::Label), " ▼");
        assert_eq!(s.marker(Key::Tokens), "");
        assert_eq!(s.press(Key::Label).marker(Key::Label), " ▲");
    }

    #[test]
    fn a_models_cost_follows_it_through_a_sort() {
        // Model costs arrive in a parallel array indexed by position. Pairing
        // them after the reorder rather than before would put one model's money
        // beside another's tokens, and every row would still look plausible.
        let models = vec![
            (Some("b-model".to_owned()), totals(1, 100)),
            (Some("a-model".to_owned()), totals(1, 900)),
        ];
        let costs = vec![money("1.00"), money("9.00")];
        let mut rows = join_models(&models, &costs, "(not reported)");
        apply(
            &mut rows,
            Sort {
                key: Key::Label,
                descending: false,
            },
        );
        assert_eq!(rows[0].label, "a-model");
        assert_eq!(rows[0].totals.grand_total(), 900);
        assert_eq!(rows[0].cost.and_then(|m| m.value), money("9.00").value);
    }

    #[test]
    fn a_model_with_no_id_still_gets_a_label() {
        let models = vec![(None, totals(1, 1))];
        let rows = join_models(&models, &[], "(not reported)");
        assert_eq!(rows[0].label, "(not reported)");
    }
}
