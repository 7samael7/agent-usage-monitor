//! `aum` — what your AI coding agents actually spent.
//!
//! Reads the transcripts Claude Code and Codex already write to disk, records
//! them, and reports what they cost. No account, no telemetry, no network, no
//! background service: one binary, one local SQLite file, two directories read.
//!
//! The rule the whole thing is built around: **a number is never presented as
//! more certain than it is.** A count the provider did not report shows as `—`
//! and not as zero; a cost derived from a rate we hold shows as `≈`; a total
//! missing one contributor shows as `≥`. That distinction is why this exists
//! rather than a shell script over `jq`.

mod cli;
mod context;
mod fmt;
mod report;
mod tui;

use clap::Parser as _;
use cli::{Cli, Command};
use context::Context;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // Logs go to stderr so that `aum daily --json | jq` stays clean.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("AUM_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .without_time()
        .init();

    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aum: {e}");
            for cause in e.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let args = Cli::parse();
    let colour = cli::use_colour(args.no_color);
    let range = args.range.resolve(args.until.as_deref())?;
    let filter = range.filter(args.adapter.as_deref());

    // Reports catch up before they report; a monitor showing yesterday's numbers
    // without saying so is worse than one that takes a moment. Writing commands
    // skip it so `aum price` is instant, and `sync` skips it because doing the
    // pass *is* the command — otherwise it runs twice and reports the second,
    // empty one.
    let own_pass = matches!(
        args.command,
        Some(Command::Price { .. } | Command::Fx { .. } | Command::Sync)
    );
    let sync = !args.no_sync && !own_pass;

    let ctx = Context::open(args.db.as_deref(), &args.currency, colour, sync).await?;

    match args.command {
        // No subcommand and a real terminal means the interactive view. With
        // `--json`, or piped somewhere, it means the overview as text — a TUI
        // written into a pipe is escape-code soup.
        None if !args.json && colour => tui::run(&ctx, &filter, &range.label).await,
        None | Some(Command::Overview) => {
            report::overview(&ctx, &filter, &range.label, args.json).await
        }
        Some(Command::Daily) => {
            report::buckets(&ctx, &filter, &range.label, false, args.json).await
        }
        Some(Command::Hourly) => {
            report::buckets(&ctx, &filter, &range.label, true, args.json).await
        }
        Some(Command::Models) => report::models(&ctx, &filter, &range.label, args.json).await,
        Some(Command::Sessions { limit }) => report::sessions(&ctx, limit, args.json).await,
        Some(Command::Apps) => report::apps(&ctx, args.json).await,
        Some(Command::Sync) => sync_once(&ctx, args.json).await,
        Some(Command::Price {
            model,
            input,
            output,
            cache_read,
            cache_write_5m,
            cache_write_1h,
            note,
        }) => {
            set_price(
                &ctx,
                &model,
                &input,
                &output,
                cache_read.as_deref(),
                cache_write_5m.as_deref(),
                cache_write_1h.as_deref(),
                note,
            )
            .await
        }
        Some(Command::Fx { currency, rate }) => set_fx(&ctx, &currency, &rate).await,
    }
}

async fn sync_once(ctx: &Context, json: bool) -> anyhow::Result<()> {
    let engine = aum_engine::Engine::new(ctx.db.clone(), &ctx.home);
    let stats = engine.pass().await;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "files_scanned": stats.files_scanned,
                "files_skipped": stats.files_skipped,
                "requests_recorded": stats.usage_recorded,
                "duplicates": stats.duplicates,
                "failures": stats.failures,
                "anomalies": stats.anomalies,
            }))?
        );
    } else {
        // "checked", not "read": cursors mean an unchanged file costs a stat
        // and nothing more, so a large number here is not a large amount of work.
        println!(
            "checked {} file(s), {} new request(s) recorded, {} already known",
            stats.files_scanned, stats.usage_recorded, stats.duplicates
        );
        if stats.failures > 0 {
            println!(
                "{} request(s) failed at the provider and have no tokens to count",
                stats.failures
            );
        }
        if stats.anomalies > 0 {
            println!(
                "{} line(s) could not be trusted and were recorded as anomalies — see the \
                 ingest_anomaly table",
                stats.anomalies
            );
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "one parameter per published rate; grouping them would only move the list"
)]
async fn set_price(
    ctx: &Context,
    model: &str,
    input: &str,
    output: &str,
    cache_read: Option<&str>,
    cache_write_5m: Option<&str>,
    cache_write_1h: Option<&str>,
    note: Option<String>,
) -> anyhow::Result<()> {
    use std::str::FromStr as _;

    let parse = |what: &str, raw: &str| -> anyhow::Result<rust_decimal::Decimal> {
        rust_decimal::Decimal::from_str(raw).map_err(|_| {
            anyhow::anyhow!("{raw} is not a valid {what} rate — expected a number like 5.00")
        })
    };

    let input_rate = parse("input", input)?;
    // Absent means "charged at the input rate", which is what both providers
    // document. It does not mean free: pricing cache reads at zero understates
    // a long cached session by most of its total.
    let rates = aum_pricing::Rates {
        input_per_mtok: input_rate,
        output_per_mtok: parse("output", output)?,
        cache_read_per_mtok: cache_read.map_or(Ok(input_rate), |r| parse("cache read", r))?,
        cache_write_5m_per_mtok: cache_write_5m
            .map_or(Ok(input_rate), |r| parse("5-minute cache write", r))?,
        cache_write_1h_per_mtok: cache_write_1h
            .map_or(Ok(input_rate), |r| parse("1-hour cache write", r))?,
    };

    if rates.input_per_mtok.is_sign_negative() || rates.output_per_mtok.is_sign_negative() {
        anyhow::bail!("a rate cannot be negative");
    }

    let saved = aum_engine::prices::save_price(&ctx.db, model, &rates, note).await?;
    println!(
        "recorded {model}: ${} in / ${} out per million tokens",
        saved.rates.input_per_mtok, saved.rates.output_per_mtok
    );
    println!("this is a new version — anything already costed keeps the rate it was costed with");
    Ok(())
}

async fn set_fx(ctx: &Context, currency: &str, rate: &str) -> anyhow::Result<()> {
    use std::str::FromStr as _;

    let quote = aum_engine::prices::parse_currency(currency).ok_or_else(|| {
        anyhow::anyhow!("{currency} is not a currency this tool can present (EUR, CZK)")
    })?;
    if quote == aum_contract::Currency::Usd {
        anyhow::bail!("USD is the base currency and needs no rate");
    }
    let value = rust_decimal::Decimal::from_str(rate)
        .map_err(|_| anyhow::anyhow!("{rate} is not a rate — expected a number like 0.92"))?;
    if value <= rust_decimal::Decimal::ZERO {
        anyhow::bail!("an exchange rate must be greater than zero");
    }

    let saved = aum_engine::prices::save_fx(&ctx.db, quote, value).await?;
    println!("recorded 1 USD = {} {}", saved.rate, saved.quote.code());
    println!("converted amounts are marked calculated, and estimated once the rate is a week old");
    Ok(())
}
