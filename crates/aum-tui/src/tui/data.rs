//! Everything the interface draws, loaded in one go.
//!
//! Queried once per refresh rather than per frame. A terminal redraws on every
//! keystroke and on every resize; running seven aggregate queries each time
//! would make the interface feel slower the more history you have, which is
//! exactly backwards.

use aum_contract::{Measured, Money, SessionSummary};
use aum_db::usage::{self, Filter, Totals};

use crate::context::Context;

/// One day of the contribution graph.
#[derive(Debug, Clone, Copy)]
pub struct Day {
    pub date: chrono::NaiveDate,
    pub tokens: i64,
    pub requests: i64,
}

pub struct Data {
    pub label: String,
    pub totals: Totals,
    pub failed: i64,
    /// Per bucket, already folded across models — for the charts and tables.
    pub daily: Vec<(String, Totals)>,
    pub daily_cost: Vec<(String, Measured<Money>)>,
    pub hourly: Vec<(String, Totals)>,
    pub hourly_cost: Vec<(String, Measured<Money>)>,
    pub models: Vec<(Option<String>, Totals)>,
    pub model_cost: Vec<Measured<Money>>,
    pub adapters: Vec<(String, Totals)>,
    pub cost: Measured<Money>,
    pub sessions: Vec<SessionSummary>,
    pub applications: Vec<aum_contract::AdapterDescriptor>,
    /// A year of days for the heatmap, oldest first, with gaps filled in.
    pub calendar: Vec<Day>,
    pub loaded_at: chrono::DateTime<chrono::Local>,
}

impl Data {
    pub async fn load(ctx: &Context, filter: &Filter, label: &str) -> anyhow::Result<Self> {
        let db = ctx.db.reader();

        let totals = usage::totals(db, filter).await?;
        let failed = usage::failed_count(db, filter).await?;
        let models = usage::by_model(db, filter).await?;
        let adapters = usage::by_adapter(db, filter).await?;

        let day_slices = usage::by_day_model(db, filter).await?;
        let daily = usage::fold_by_bucket(&day_slices);
        let daily_cost = aum_engine::prices::cost_by_bucket(&day_slices, &ctx.money.table)
            .into_iter()
            .map(|(at, m)| (at, ctx.cost().present(m)))
            .collect();

        let hour_slices = usage::by_hour_model(db, filter).await?;
        let hourly = usage::fold_by_bucket(&hour_slices);
        let hourly_cost = aum_engine::prices::cost_by_bucket(&hour_slices, &ctx.money.table)
            .into_iter()
            .map(|(at, m)| (at, ctx.cost().present(m)))
            .collect();

        let cost = ctx.cost().present(aum_engine::prices::cost_of_slices(
            models.iter().map(|(m, t)| (m, t)),
            &ctx.money.table,
        ));
        let model_cost = models
            .iter()
            .map(|(m, t)| {
                ctx.cost().present(aum_engine::prices::cost_of_slices(
                    std::iter::once((m, t)),
                    &ctx.money.table,
                ))
            })
            .collect();

        let sessions = aum_db::repo::recent_sessions(db, 200)
            .await?
            .into_iter()
            .map(aum_engine::views::session_summary)
            .collect();

        // The heatmap always shows a year, whatever range the rest is showing:
        // it exists to give the other tabs context, and a heatmap of the same
        // seven days you are already looking at tells you nothing.
        let year_ago = chrono::Local::now().date_naive() - chrono::Duration::days(364);
        let year_filter = Filter {
            since: Some(aum_db::to_sql_time(
                year_ago.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc(),
            )),
            until: None,
            adapter: filter.adapter.clone(),
        };
        let calendar = calendar(
            &usage::fold_by_bucket(&usage::by_day_model(db, &year_filter).await?),
            year_ago,
        );

        let desktop_daily = aum_db::desktop::daily_history(db, 30)
            .await
            .map(|rows| aum_engine::daily_total_from(&rows))
            .unwrap_or_default();
        let home = ctx.home.clone();
        let applications = tokio::task::spawn_blocking(move || {
            aum_engine::views::describe_adapters(&home, desktop_daily)
        })
        .await
        .unwrap_or_default();

        Ok(Self {
            label: label.to_owned(),
            totals,
            failed,
            daily,
            daily_cost,
            hourly,
            hourly_cost,
            models,
            model_cost,
            adapters,
            cost,
            sessions,
            applications,
            calendar,
            loaded_at: chrono::Local::now(),
        })
    }
}

/// Fill every date from `start` to today, so a quiet day is drawn as a quiet
/// day rather than closing the gap and making the calendar lie about when work
/// happened.
fn calendar(buckets: &[(String, Totals)], start: chrono::NaiveDate) -> Vec<Day> {
    use std::collections::HashMap;
    let by_date: HashMap<&str, &Totals> = buckets.iter().map(|(at, t)| (at.as_str(), t)).collect();

    let today = chrono::Local::now().date_naive();
    let mut out = Vec::new();
    let mut date = start;
    while date <= today {
        let key = date.format("%Y-%m-%d").to_string();
        let (tokens, requests) = by_date
            .get(key.as_str())
            .map_or((0, 0), |t| (t.grand_total(), t.requests));
        out.push(Day {
            date,
            tokens,
            requests,
        });
        date = match date.succ_opt() {
            Some(d) => d,
            None => break,
        };
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn quiet_days_are_kept_rather_than_closed_up() {
        // A calendar that skipped empty days would draw a continuous streak
        // where there were gaps — the one thing a contribution graph is for.
        let start = chrono::Local::now().date_naive() - chrono::Duration::days(4);
        let busy = (start + chrono::Duration::days(2))
            .format("%Y-%m-%d")
            .to_string();
        let buckets = vec![(
            busy.clone(),
            Totals {
                requests: 3,
                output_total: 100,
                ..Totals::default()
            },
        )];

        let cal = calendar(&buckets, start);
        assert_eq!(cal.len(), 5, "five days including today");
        assert_eq!(cal.iter().filter(|d| d.tokens > 0).count(), 1);
        assert_eq!(cal[2].tokens, 100);
        assert_eq!(cal[0].tokens, 0);
        assert_eq!(cal[0].requests, 0);
    }

    #[test]
    fn the_calendar_runs_up_to_today_inclusive() {
        let start = chrono::Local::now().date_naive();
        let cal = calendar(&[], start);
        assert_eq!(cal.len(), 1);
        assert_eq!(cal[0].date, chrono::Local::now().date_naive());
    }
}
