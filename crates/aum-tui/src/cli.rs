//! The command line.
//!
//! Every subcommand accepts the same range and filter flags, and every one can
//! emit `--json`, so the tool is scriptable without a second interface. The
//! interactive view is what you get with no subcommand at all.

use chrono::{Local, NaiveDate, TimeZone as _, Utc};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "aum",
    about = "What your AI coding agents actually spent",
    long_about = "Reads the transcripts Claude Code and Codex already write, and reports what \
                  they cost — without ever presenting a number as more certain than it is.",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub range: Range,

    /// Up to (but not including) this instant. Bounds `--since`.
    #[arg(long, global = true, value_name = "DATE")]
    pub until: Option<String>,

    /// Only this agent: claude_code or codex.
    #[arg(long, short, global = true, value_name = "ID")]
    pub adapter: Option<String>,

    /// Show amounts in this currency. Conversion needs a rate you have entered.
    #[arg(long, short, global = true, value_name = "CODE", default_value = "USD")]
    pub currency: String,

    /// Emit JSON instead of a table.
    #[arg(long, global = true)]
    pub json: bool,

    /// Never colour the output. `NO_COLOR` in the environment does the same.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Read a database somewhere other than the default location.
    #[arg(long, global = true, value_name = "PATH")]
    pub db: Option<std::path::PathBuf>,

    /// Do not read new transcripts before reporting; use what is already stored.
    #[arg(long, global = true)]
    pub no_sync: bool,

    /// Order the rows of `daily`, `hourly` and `models`.
    #[arg(long, global = true, value_name = "COLUMN", value_enum)]
    pub sort: Option<SortColumn>,

    /// Reverse whatever order is in force.
    #[arg(long, global = true)]
    pub reverse: bool,
}

/// The column a table may be ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SortColumn {
    /// Date, hour, or model name.
    Date,
    Requests,
    Tokens,
    /// Rows with no price sort last either way — they are not cheap rows.
    Cost,
}

impl Cli {
    /// The order for a table, and what it is called.
    ///
    /// Chronological by default, unlike the interactive view, and for a reason
    /// rather than an oversight: printed output scrolls, so the *last* line is
    /// the one left beside your prompt. Oldest first puts today there. In a
    /// fixed viewport the first row is the one you see, so the interface
    /// defaults the other way.
    #[must_use]
    pub fn sort_order(&self, default: crate::sort::Sort) -> crate::sort::Sort {
        use crate::sort::Key;
        let mut sort = match self.sort {
            Some(SortColumn::Date) => crate::sort::Sort {
                key: Key::Label,
                descending: false,
            },
            Some(SortColumn::Requests) => crate::sort::Sort {
                key: Key::Requests,
                descending: true,
            },
            Some(SortColumn::Tokens) => crate::sort::Sort {
                key: Key::Tokens,
                descending: true,
            },
            Some(SortColumn::Cost) => crate::sort::Sort {
                key: Key::Cost,
                descending: true,
            },
            None => default,
        };
        if self.reverse {
            sort.descending = !sort.descending;
        }
        sort
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Headline totals and the busiest models (the default view).
    Overview,
    /// Usage per day.
    Daily,
    /// Usage per hour.
    Hourly,
    /// Usage per model, and which models have no price.
    Models,
    /// Recent agent sessions.
    Sessions {
        /// How many to show.
        #[arg(long, default_value_t = 30)]
        limit: i64,
    },
    /// What each installed application can actually report.
    Apps,
    /// Record a price, in money per million tokens.
    Price {
        /// Model id exactly as the agent reports it, e.g. claude-opus-5.
        model: String,
        #[arg(long)]
        input: String,
        #[arg(long)]
        output: String,
        /// Defaults to the input rate, which is both providers' documented
        /// behaviour. It does not default to zero: that would price caching as
        /// free and understate a long session by most of its total.
        #[arg(long)]
        cache_read: Option<String>,
        #[arg(long)]
        cache_write_5m: Option<String>,
        #[arg(long)]
        cache_write_1h: Option<String>,
        /// Where the figure came from, kept with the price.
        #[arg(long)]
        note: Option<String>,
    },
    /// Record an exchange rate, in units of a currency per 1 USD.
    Fx {
        /// EUR or CZK. USD is the base and needs no rate.
        currency: String,
        /// For example 0.92.
        rate: String,
    },
    /// Read new transcripts and exit. Useful from cron.
    Sync,
}

/// Which slice of history to report on.
///
/// The shortcuts are mutually exclusive with each other but compose with
/// nothing else, which keeps "since last Tuesday but also this month" from
/// being expressible at all.
#[derive(Debug, Clone, clap::Args)]
#[group(multiple = false)]
pub struct Range {
    /// Since this instant. `YYYY-MM-DD` or a full RFC 3339 timestamp.
    #[arg(long, global = true, value_name = "DATE", group = "span")]
    pub since: Option<String>,

    /// Today only.
    #[arg(long, global = true, group = "span")]
    pub today: bool,

    /// The last seven days.
    #[arg(long, global = true, group = "span")]
    pub week: bool,

    /// The last thirty days.
    #[arg(long, global = true, group = "span")]
    pub month: bool,

    /// A calendar year.
    #[arg(long, global = true, value_name = "YYYY", group = "span")]
    pub year: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub since: Option<String>,
    pub until: Option<String>,
    /// How to describe the range in a header, in words.
    pub label: String,
}

/// Local midnight on a date, as a UTC timestamp string.
///
/// Days are the user's days. A boundary computed in UTC would put an evening's
/// work in tomorrow for anyone west of Greenwich, and split a working day in
/// two for anyone east of it.
fn local_midnight(date: NaiveDate) -> Option<String> {
    let naive = date.and_hms_opt(0, 0, 0)?;
    let local = Local.from_local_datetime(&naive).earliest()?;
    Some(aum_db::to_sql_time(local.with_timezone(&Utc)))
}

/// Parse a `YYYY-MM-DD` or RFC 3339 argument into a UTC timestamp string.
fn parse_boundary(raw: &str) -> anyhow::Result<String> {
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return local_midnight(date)
            .ok_or_else(|| anyhow::anyhow!("{raw} is not a date this timezone has"));
    }
    let ts = chrono::DateTime::parse_from_rfc3339(raw)
        .map_err(|_| anyhow::anyhow!("{raw} is not a date (expected YYYY-MM-DD) or a timestamp"))?;
    Ok(aum_db::to_sql_time(ts.with_timezone(&Utc)))
}

impl Range {
    /// Turn the flags into a concrete half-open range.
    pub fn resolve(&self, until: Option<&str>) -> anyhow::Result<Resolved> {
        let today = Local::now().date_naive();

        let (since, mut end, label) = if self.today {
            (
                local_midnight(today),
                local_midnight(today.succ_opt().unwrap_or(today)),
                "today".to_owned(),
            )
        } else if self.week {
            (
                local_midnight(today - chrono::Duration::days(6)),
                None,
                "the last 7 days".to_owned(),
            )
        } else if self.month {
            (
                local_midnight(today - chrono::Duration::days(29)),
                None,
                "the last 30 days".to_owned(),
            )
        } else if let Some(year) = self.year {
            let start = NaiveDate::from_ymd_opt(year, 1, 1)
                .ok_or_else(|| anyhow::anyhow!("{year} is not a year"))?;
            let next = NaiveDate::from_ymd_opt(year + 1, 1, 1)
                .ok_or_else(|| anyhow::anyhow!("{year} is not a year"))?;
            (
                local_midnight(start),
                local_midnight(next),
                format!("{year}"),
            )
        } else if let Some(raw) = &self.since {
            let at = parse_boundary(raw)?;
            (Some(at), None, format!("since {raw}"))
        } else {
            (None, None, "all time".to_owned())
        };

        // An explicit --until always wins; the shortcuts only supply one when
        // they are inherently bounded, like --today or --year.
        let mut label = label;
        if let Some(raw) = until {
            end = Some(parse_boundary(raw)?);
            // The header has to name both ends, or a bounded range reads as an
            // open one and the numbers look wrong for the period stated.
            label = if self.since.is_some() {
                format!("{label} until {raw}")
            } else {
                format!("{label}, until {raw}")
            };
        }

        Ok(Resolved {
            since,
            until: end,
            label,
        })
    }
}

impl Resolved {
    #[must_use]
    pub fn filter(&self, adapter: Option<&str>) -> aum_db::usage::Filter {
        let mut f = aum_db::usage::Filter::default();
        f.since.clone_from(&self.since);
        f.until.clone_from(&self.until);
        f.adapter = adapter.map(str::to_owned);
        f
    }
}

/// Whether to colour the output.
///
/// `NO_COLOR` is honoured because it is the convention, and a pipe is left
/// uncoloured because escape codes in a file someone is grepping are noise.
#[must_use]
pub fn use_colour(flag: bool) -> bool {
    use std::io::IsTerminal as _;
    !flag && std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn a_bare_date_is_read_as_local_midnight() {
        let at = parse_boundary("2026-08-13").unwrap();
        // Whatever the offset, it is a valid stored timestamp and it is the
        // start of a day somewhere — not a naive string comparison failure.
        assert!(at.ends_with('Z'), "{at}");
        assert!(at.contains("2026-08-1"), "{at}");
    }

    #[test]
    fn a_full_timestamp_is_accepted_unchanged_in_meaning() {
        let at = parse_boundary("2026-08-13T09:00:00Z").unwrap();
        assert_eq!(at, "2026-08-13T09:00:00.000Z");
    }

    #[test]
    fn nonsense_is_refused_with_a_message_naming_the_format() {
        let err = parse_boundary("last tuesday").unwrap_err().to_string();
        assert!(err.contains("YYYY-MM-DD"), "{err}");
    }

    #[test]
    fn no_flags_means_all_of_history() {
        let r = Range {
            since: None,
            today: false,
            week: false,
            month: false,
            year: None,
        }
        .resolve(None)
        .unwrap();
        assert_eq!(r.since, None);
        assert_eq!(r.until, None);
        assert_eq!(r.label, "all time");
    }

    #[test]
    fn today_is_bounded_at_both_ends() {
        // Open-ended would sweep in anything with a clock-skewed future
        // timestamp, which transcripts occasionally have.
        let r = Range {
            since: None,
            today: true,
            week: false,
            month: false,
            year: None,
        }
        .resolve(None)
        .unwrap();
        assert!(r.since.is_some());
        assert!(r.until.is_some());
        assert!(r.since < r.until);
    }

    #[test]
    fn a_year_covers_exactly_that_year() {
        let r = Range {
            since: None,
            today: false,
            week: false,
            month: false,
            year: Some(2026),
        }
        .resolve(None)
        .unwrap();
        // Local midnight on 1 January, which in a positive-offset zone is the
        // last hours of 31 December once converted to UTC.
        let since = r.since.as_deref().expect("a year has a start");
        assert!(
            since.contains("2026-01-01") || since.contains("2025-12-31"),
            "{since}"
        );
        assert!(r.until.is_some(), "a year is bounded at both ends");
        assert_eq!(r.label, "2026");
    }

    #[test]
    fn an_explicit_until_overrides_a_shortcuts_own_bound() {
        let r = Range {
            since: None,
            today: false,
            week: true,
            month: false,
            year: None,
        }
        .resolve(Some("2026-08-13"))
        .unwrap();
        assert!(r.until.is_some());
    }

    fn parse(args: &[&str]) -> Cli {
        use clap::Parser as _;
        Cli::try_parse_from(std::iter::once("aum").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn printed_output_is_chronological_unless_asked_otherwise() {
        // Different from the interactive default, and deliberately: printed
        // rows scroll, so the last one is the one left beside the prompt.
        let sort = parse(&["daily"]).sort_order(crate::sort::Sort::OLDEST_FIRST);
        assert_eq!(sort, crate::sort::Sort::OLDEST_FIRST);
    }

    #[test]
    fn reverse_on_its_own_flips_whatever_the_default_was() {
        // The short way to say "today first" without naming a column.
        let sort = parse(&["daily", "--reverse"]).sort_order(crate::sort::Sort::OLDEST_FIRST);
        assert_eq!(sort, crate::sort::Sort::NEWEST_FIRST);
    }

    #[test]
    fn reverse_also_flips_an_explicit_column() {
        let dearest =
            parse(&["models", "--sort", "cost"]).sort_order(crate::sort::Sort::OLDEST_FIRST);
        let cheapest = parse(&["models", "--sort", "cost", "--reverse"])
            .sort_order(crate::sort::Sort::OLDEST_FIRST);
        assert_eq!(dearest.key, crate::sort::Key::Cost);
        assert!(dearest.descending);
        assert!(!cheapest.descending);
    }

    #[test]
    fn a_named_column_ignores_the_default_entirely() {
        let sort =
            parse(&["daily", "--sort", "tokens"]).sort_order(crate::sort::Sort::LARGEST_FIRST);
        assert_eq!(sort.key, crate::sort::Key::Tokens);
        assert!(sort.descending, "a quantity starts with the biggest");
    }

    #[test]
    fn an_unknown_sort_column_is_refused_rather_than_ignored() {
        use clap::Parser as _;
        assert!(Cli::try_parse_from(["aum", "daily", "--sort", "vibes"]).is_err());
    }
}
