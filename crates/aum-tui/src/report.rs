//! The reports, as text or as JSON.
//!
//! Each one builds the same data twice over: a `serde_json::Value` for
//! `--json`, and a table for a person. They are built from one query result so
//! the two cannot drift.

use aum_contract::{Measured, Money};
use aum_db::usage::{self, Filter, Totals};
use serde_json::json;

use crate::context::Context;
use crate::fmt::{self, Align, Table};

/// What a model with no reported id is called. One spelling, shared with the
/// interactive view, so the two tables never disagree about the same row.
pub const NOT_REPORTED: &str = "(not reported)";

/// The certainty as a stable JSON token. A machine reader needs a word it can
/// match on, and it must be the same word every release.
const fn kind_name(kind: aum_contract::DisplayKind) -> &'static str {
    use aum_contract::DisplayKind as K;
    match kind {
        K::Exact => "exact",
        K::Calculated => "calculated",
        K::Estimated => "estimated",
        K::Partial => "partial",
        K::Unavailable => "unavailable",
    }
}

const fn adapter_state(state: &aum_contract::AdapterState) -> &'static str {
    use aum_contract::AdapterState as S;
    match state {
        S::Ready => "reporting usage",
        S::Detected => "installed",
        S::NotInstalled => "not found",
        S::Error => "error",
    }
}

fn measured_money_json(m: &Measured<Money>, currency: &str) -> serde_json::Value {
    json!({
        // A decimal string, never a JSON number: a double loses precision the
        // moment sub-cent per-request costs are summed.
        "value": m.value.as_ref().map(std::string::ToString::to_string),
        "currency": currency,
        "accuracy": kind_name(m.accuracy.display_kind()),
    })
}

fn totals_json(t: &Totals) -> serde_json::Value {
    json!({
        "requests": t.requests,
        "input_fresh": t.input_fresh,
        "cache_read": t.cache_read,
        "cache_write_5m": t.cache_write_5m,
        "cache_write_1h": t.cache_write_1h,
        "cache_write_unspecified": t.cache_write_unspecified,
        "output_total": t.output_total,
        "unclassified": t.unclassified,
        // Null, not zero, where nobody reported it.
        "reasoning": t.reasoning,
        "reasoning_reported_by": t.reasoning_reported_by,
        "total_tokens": t.grand_total(),
    })
}

/// Reasoning as a `Measured`, so an agent that does not report it renders as a
/// dash rather than as zero, and a partly-reporting set renders as a floor.
pub fn reasoning_measure(t: &Totals) -> Measured<u64> {
    use aum_contract::{MeasurementSource, UnavailableReason};
    let n = |v: i64| u64::try_from(v).unwrap_or(0);
    match t.reasoning {
        None => Measured::unavailable(UnavailableReason::NotReportedByProvider {
            field: "reasoning_tokens".to_owned(),
            detail: "No request in this range reported a reasoning-token count.".to_owned(),
        }),
        Some(v) if t.reasoning_reported_by >= t.requests => {
            Measured::exact(n(v), MeasurementSource::ProviderReported)
        }
        Some(v) => Measured::partial(
            n(v),
            u32::try_from(t.reasoning_reported_by).unwrap_or(u32::MAX),
            u32::try_from(t.requests).unwrap_or(u32::MAX),
            format!(
                "{} of {} requests report reasoning tokens",
                t.reasoning_reported_by, t.requests
            ),
        ),
    }
}

// ── Overview ────────────────────────────────────────────────────────────────

pub async fn overview(
    ctx: &Context,
    filter: &Filter,
    label: &str,
    json: bool,
) -> anyhow::Result<()> {
    let totals = usage::totals(ctx.db.reader(), filter).await?;
    let failed = usage::failed_count(ctx.db.reader(), filter).await?;
    let per_model = usage::by_model(ctx.db.reader(), filter).await?;
    let per_adapter = usage::by_adapter(ctx.db.reader(), filter).await?;

    let cost = ctx.cost().present(aum_engine::prices::cost_of_slices(
        per_model.iter().map(|(m, t)| (m, t)),
        &ctx.money.table,
    ));

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "range": label,
                "totals": totals_json(&totals),
                "failed_requests": failed,
                "cost": measured_money_json(&cost, ctx.currency_code()),
                "by_adapter": per_adapter.iter().map(|(a, t)| json!({
                    "adapter": a, "totals": totals_json(t)
                })).collect::<Vec<_>>(),
                "by_model": per_model.iter().map(|(m, t)| json!({
                    "model": m, "totals": totals_json(t)
                })).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }

    let c = ctx.colour;
    println!("{}", heading(&format!("Usage — {label}"), c));
    println!();
    println!(
        "  {:<14} {}",
        "requests",
        fmt::thousands(totals.requests)
            + &(if failed > 0 {
                format!(
                    "   ({} failed and could not be measured)",
                    fmt::thousands(failed)
                )
            } else {
                String::new()
            })
    );
    println!(
        "  {:<14} {}",
        "total tokens",
        fmt::thousands(totals.grand_total())
    );
    println!(
        "  {:<14} {}",
        "input side",
        fmt::thousands(totals.input_side())
    );
    println!("  {:<14} {}", "output", fmt::thousands(totals.output_total));
    println!(
        "  {:<14} {}",
        "reasoning",
        fmt::render_tokens(&reasoning_measure(&totals), c)
    );
    println!(
        "  {:<14} {}",
        "API-equivalent",
        fmt::render_money(&cost, ctx.currency_code(), c)
    );
    println!(
        "  {:<14} {}",
        "actually billed",
        if c {
            format!(
                "{}—  subscription, not billed per token{}",
                fmt::DIM,
                fmt::RESET
            )
        } else {
            "—  subscription, not billed per token".to_owned()
        }
    );

    if !per_adapter.is_empty() {
        println!("\n{}", heading("By agent", c));
        let mut t = Table::new(&[
            ("agent", Align::Left),
            ("requests", Align::Right),
            ("tokens", Align::Right),
            ("output", Align::Right),
        ]);
        for (adapter, totals) in &per_adapter {
            t.push(vec![
                adapter.clone(),
                fmt::thousands(totals.requests),
                fmt::thousands(totals.grand_total()),
                fmt::thousands(totals.output_total),
            ]);
        }
        print!("{}", t.render(c));
    }

    if !per_model.is_empty() {
        println!("\n{}", heading("Top models", c));
        // Biggest first, always: the overview's job is to say where it went.
        let costs = model_costs(ctx, &per_model);
        let mut rows = crate::sort::join_models(&per_model, &costs, NOT_REPORTED);
        crate::sort::apply(&mut rows, crate::sort::Sort::LARGEST_FIRST);
        print!("{}", model_table(ctx, &rows, 8));
    }

    println!("\n{}", fmt::legend(c));
    warn_unpriced(ctx, &per_model);
    Ok(())
}

// ── Daily and hourly ────────────────────────────────────────────────────────

pub async fn buckets(
    ctx: &Context,
    filter: &Filter,
    label: &str,
    hourly: bool,
    json: bool,
    sort: crate::sort::Sort,
) -> anyhow::Result<()> {
    let slices = if hourly {
        usage::by_hour_model(ctx.db.reader(), filter).await?
    } else {
        usage::by_day_model(ctx.db.reader(), filter).await?
    };
    let folded = usage::fold_by_bucket(&slices);
    // Costed per model within each bucket, then summed — never from the
    // bucket's blended tokens.
    let costs: Vec<_> = aum_engine::prices::cost_by_bucket(&slices, &ctx.money.table)
        .into_iter()
        .map(|(at, m)| (at, ctx.cost().present(m)))
        .collect();

    let mut rows = crate::sort::join(&folded, &costs);
    crate::sort::apply(&mut rows, sort);

    let unmeasured = Measured::unavailable(aum_contract::UnavailableReason::NoTelemetry {
        detail: "no usage in this bucket".to_owned(),
    });

    if json {
        let out: Vec<_> = rows
            .iter()
            .map(|r| {
                json!({
                    "at": r.label,
                    "totals": totals_json(r.totals),
                    "cost": measured_money_json(r.cost.unwrap_or(&unmeasured), ctx.currency_code()),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "range": label,
                "order": sort.describe(crate::sort::Labels::Time),
                "buckets": out,
            }))?
        );
        return Ok(());
    }

    let c = ctx.colour;
    let unit = if hourly { "Hourly" } else { "Daily" };
    println!("{}", heading(&format!("{unit} usage — {label}"), c));
    println!();

    if rows.is_empty() {
        println!("  Nothing recorded in this range.");
        return Ok(());
    }

    // The bar is scaled to the largest row, whatever order they are printed in,
    // so re-sorting never changes how long a given day's bar is.
    let peak = rows
        .iter()
        .map(|r| r.totals.grand_total())
        .max()
        .unwrap_or(1)
        .max(1);

    let mut t = Table::new(&[
        (if hourly { "hour" } else { "day" }, Align::Left),
        ("requests", Align::Right),
        ("input", Align::Right),
        ("output", Align::Right),
        ("tokens", Align::Right),
        ("cost", Align::Right),
        ("", Align::Left),
    ]);
    for r in &rows {
        t.push(vec![
            r.label.replace('T', " "),
            fmt::thousands(r.totals.requests),
            fmt::thousands(r.totals.input_side()),
            fmt::thousands(r.totals.output_total),
            fmt::thousands(r.totals.grand_total()),
            fmt::render_money(r.cost.unwrap_or(&unmeasured), ctx.currency_code(), c),
            bar(r.totals.grand_total(), peak, 24, c),
        ]);
    }
    print!("{}", t.render(c));
    // The order of one row is not information.
    println!(
        "\n{}{}",
        fmt::legend(c),
        order_note(rows.len(), sort, crate::sort::Labels::Time)
    );
    Ok(())
}

/// A proportional bar. Scaled to the largest bucket in view, which is stated in
/// the caption so the width is never mistaken for an absolute quantity.
fn bar(value: i64, peak: i64, width: usize, colourise: bool) -> String {
    if value <= 0 {
        return String::new();
    }
    let filled = ((value as f64 / peak as f64) * width as f64)
        .round()
        .max(1.0) as usize;
    let body = "█".repeat(filled.min(width));
    if colourise {
        format!("\x1b[38;5;110m{body}{}", fmt::RESET)
    } else {
        body
    }
}

// ── Models ──────────────────────────────────────────────────────────────────

pub async fn models(
    ctx: &Context,
    filter: &Filter,
    label: &str,
    json: bool,
    sort: crate::sort::Sort,
) -> anyhow::Result<()> {
    let per_model = usage::by_model(ctx.db.reader(), filter).await?;

    if json {
        let rows: Vec<_> = per_model
            .iter()
            .map(|(model, t)| {
                let cost = ctx.cost().present(aum_engine::prices::cost_of_slices(
                    std::iter::once((model, t)),
                    &ctx.money.table,
                ));
                let rate = model
                    .as_deref()
                    .and_then(|m| ctx.money.table.lookup(m))
                    .map(|p| {
                        json!({
                            "input_per_mtok": p.rates.input_per_mtok.to_string(),
                            "output_per_mtok": p.rates.output_per_mtok.to_string(),
                            "source": p.source,
                        })
                    });
                json!({
                    "model": model,
                    "totals": totals_json(t),
                    "cost": measured_money_json(&cost, ctx.currency_code()),
                    "rate": rate,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "range": label,
                "order": sort.describe(crate::sort::Labels::Name),
                "models": rows,
            }))?
        );
        return Ok(());
    }

    let c = ctx.colour;
    println!("{}", heading(&format!("Models — {label}"), c));
    println!();
    if per_model.is_empty() {
        println!("  Nothing recorded in this range.");
        return Ok(());
    }
    let costs = model_costs(ctx, &per_model);
    let mut rows = crate::sort::join_models(&per_model, &costs, NOT_REPORTED);
    crate::sort::apply(&mut rows, sort);
    print!("{}", model_table(ctx, &rows, usize::MAX));
    println!(
        "\n{}{}",
        fmt::legend(c),
        order_note(rows.len(), sort, crate::sort::Labels::Name)
    );
    warn_unpriced(ctx, &per_model);
    Ok(())
}

/// The order, named — but only where there is an order to name.
fn order_note(rows: usize, sort: crate::sort::Sort, labels: crate::sort::Labels) -> String {
    if rows < 2 {
        return String::new();
    }
    format!("   ({})", sort.describe(labels))
}

/// Cost each model on its own, in the order the models arrive.
fn model_costs(ctx: &Context, per_model: &[(Option<String>, Totals)]) -> Vec<Measured<Money>> {
    per_model
        .iter()
        .map(|(model, totals)| {
            ctx.cost().present(aum_engine::prices::cost_of_slices(
                std::iter::once((model, totals)),
                &ctx.money.table,
            ))
        })
        .collect()
}

fn model_table(ctx: &Context, rows: &[crate::sort::Row<'_>], limit: usize) -> String {
    let c = ctx.colour;
    let uncosted = Measured::unavailable(aum_contract::UnavailableReason::NoTelemetry {
        detail: "not costed".to_owned(),
    });
    let mut t = Table::new(&[
        ("model", Align::Left),
        ("requests", Align::Right),
        ("tokens", Align::Right),
        ("reasoning", Align::Right),
        ("cost", Align::Right),
        ("rate /Mtok (USD)", Align::Left),
    ]);
    for r in rows.iter().take(limit) {
        let cost = r.cost.unwrap_or(&uncosted);
        // A row whose model was never reported looks up nothing, which is the
        // right answer: an unnamed model cannot have a price.
        let rate = ctx.money.table.lookup(r.label).map_or_else(
            || {
                if c {
                    format!(
                        "{}not priced{}",
                        fmt::colour(aum_contract::DisplayKind::Estimated),
                        fmt::RESET
                    )
                } else {
                    "not priced".to_owned()
                }
            },
            // Always in USD, whatever the display currency: rates are what
            // the provider publishes, and only the computed cost is
            // converted. Showing "€5 in" would claim the provider charges
            // five euros, which it does not.
            |p| {
                format!(
                    "${} in / ${} out",
                    p.rates.input_per_mtok, p.rates.output_per_mtok
                )
            },
        );
        t.push(vec![
            r.label.to_owned(),
            fmt::thousands(r.totals.requests),
            fmt::thousands(r.totals.grand_total()),
            fmt::render_tokens(&reasoning_measure(r.totals), c),
            fmt::render_money(cost, ctx.currency_code(), c),
            rate,
        ]);
    }
    t.render(c)
}

fn warn_unpriced(ctx: &Context, per_model: &[(Option<String>, Totals)]) {
    let missing: Vec<&str> = per_model
        .iter()
        .filter_map(|(m, _)| m.as_deref())
        .filter(|m| !ctx.money.table.has_price(m))
        .collect();
    if missing.is_empty() {
        return;
    }
    println!(
        "\n{}, so {} cost reads as — rather than as zero:",
        if missing.len() == 1 {
            "1 of these models has no price".to_owned()
        } else {
            format!("{} of these models have no price", missing.len())
        },
        if missing.len() == 1 { "its" } else { "their" }
    );
    for m in &missing {
        println!("    aum price {m} --input <rate> --output <rate>");
    }
}

// ── Sessions ────────────────────────────────────────────────────────────────

pub async fn sessions(ctx: &Context, limit: i64, json: bool) -> anyhow::Result<()> {
    let rows = aum_db::repo::recent_sessions(ctx.db.reader(), limit).await?;
    let summaries: Vec<_> = rows
        .into_iter()
        .map(aum_engine::views::session_summary)
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&summaries)?);
        return Ok(());
    }

    let c = ctx.colour;
    println!("{}", heading("Recent sessions", c));
    println!();
    if summaries.is_empty() {
        println!("  No sessions recorded yet.");
        return Ok(());
    }

    let mut t = Table::new(&[
        ("last active", Align::Left),
        ("agent", Align::Left),
        ("model", Align::Left),
        ("requests", Align::Right),
        ("tokens", Align::Right),
        ("reasoning", Align::Right),
    ]);
    for s in &summaries {
        t.push(vec![
            s.last_at.as_deref().map_or_else(
                || "—".to_owned(),
                |a| a.replace('T', " ").chars().take(16).collect(),
            ),
            s.adapter_id.clone(),
            s.model_id.clone().unwrap_or_else(|| "—".to_owned()),
            if s.failed > 0 {
                format!(
                    "{} +{} failed",
                    fmt::thousands(i64::from(s.requests)),
                    s.failed
                )
            } else {
                fmt::thousands(i64::from(s.requests))
            },
            fmt::render_tokens(&s.total_tokens, c),
            fmt::render_tokens(&s.reasoning_tokens, c),
        ]);
    }
    print!("{}", t.render(c));
    println!("\n{}", fmt::legend(c));
    Ok(())
}

// ── Applications ────────────────────────────────────────────────────────────

pub async fn apps(ctx: &Context, json: bool) -> anyhow::Result<()> {
    let daily = aum_db::desktop::daily_history(ctx.db.reader(), 30)
        .await
        .map(|rows| aum_engine::daily_total_from(&rows))
        .unwrap_or_default();

    let home = ctx.home.clone();
    let descriptors =
        tokio::task::spawn_blocking(move || aum_engine::views::describe_adapters(&home, daily))
            .await
            .map_err(|e| anyhow::anyhow!("could not probe the applications: {e}"))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&descriptors)?);
        return Ok(());
    }

    let c = ctx.colour;
    println!("{}", heading("Applications", c));
    println!(
        "{}",
        dim(
            "What each one can actually report, read from its own files rather than from a list.",
            c
        )
    );

    for d in &descriptors {
        println!(
            "\n{}  {}",
            bold(&d.display_name, c),
            dim(adapter_state(&d.state), c)
        );
        if let Some(path) = &d.executable_path {
            println!("  {}", dim(path, c));
        }
        for (name, state) in &d.capabilities {
            let (mark, detail) = match state {
                aum_contract::CapabilityState::Supported { evidence } => ("yes", evidence.as_str()),
                aum_contract::CapabilityState::Degraded { caveat, .. } => ("~  ", caveat.as_str()),
                aum_contract::CapabilityState::Unsupported { reason } => ("no ", reason.as_str()),
                aum_contract::CapabilityState::Unknown { reason } => ("?  ", reason.as_str()),
            };
            println!("    {mark}  {name:<28} {}", dim(&truncate(detail, 76), c));
        }
        if let Some(total) = &d.daily_total {
            println!(
                "    {}  {} on {}",
                bold("daily total", c),
                fmt::render_tokens(&total.tokens, c),
                total.day
            );
            println!("       {}", dim(&total.scope, c));
        }
    }
    println!();
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ── Small helpers ───────────────────────────────────────────────────────────

fn heading(text: &str, colourise: bool) -> String {
    if colourise {
        format!("{}{text}{}", fmt::BOLD, fmt::RESET)
    } else {
        text.to_owned()
    }
}

fn bold(text: &str, colourise: bool) -> String {
    heading(text, colourise)
}

fn dim(text: &str, colourise: bool) -> String {
    if colourise {
        format!("{}{text}{}", fmt::DIM, fmt::RESET)
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn reasoning_is_unavailable_when_no_request_reported_it() {
        let t = Totals {
            requests: 5,
            reasoning: None,
            reasoning_reported_by: 0,
            ..Totals::default()
        };
        assert_eq!(reasoning_measure(&t).value, None);
    }

    #[test]
    fn reasoning_is_a_floor_when_only_some_requests_reported_it() {
        let t = Totals {
            requests: 5,
            reasoning: Some(35),
            reasoning_reported_by: 2,
            ..Totals::default()
        };
        let m = reasoning_measure(&t);
        assert_eq!(m.value, Some(35));
        assert_eq!(
            m.accuracy.display_kind(),
            aum_contract::DisplayKind::Partial
        );
    }

    #[test]
    fn a_bar_never_vanishes_for_a_nonzero_value() {
        // A day with real usage that rounds to zero width would read as a day
        // with none.
        assert_eq!(bar(0, 100, 20, false), "");
        assert_eq!(bar(1, 1_000_000, 20, false).chars().count(), 1);
        assert_eq!(bar(100, 100, 20, false).chars().count(), 20);
    }

    #[test]
    fn money_in_json_is_a_string_so_it_cannot_become_a_float() {
        let m = Measured::calculated(
            Money::new(rust_decimal::Decimal::new(4125, 6)),
            aum_contract::MeasurementSource::ApplicationTelemetry,
        );
        let v = measured_money_json(&m, "USD");
        assert_eq!(v["value"], serde_json::Value::String("0.004125".to_owned()));
    }

    #[test]
    fn an_unavailable_amount_is_null_in_json_rather_than_zero() {
        let m: Measured<Money> =
            Measured::unavailable(aum_contract::UnavailableReason::NoPricingForModel {
                model_id: "m".to_owned(),
            });
        let v = measured_money_json(&m, "USD");
        assert_eq!(v["value"], serde_json::Value::Null);
        assert_eq!(v["accuracy"], "unavailable");
    }

    #[test]
    fn unreported_reasoning_is_null_in_json_rather_than_zero() {
        let t = Totals {
            requests: 3,
            reasoning: None,
            ..Totals::default()
        };
        assert_eq!(totals_json(&t)["reasoning"], serde_json::Value::Null);
    }
}
