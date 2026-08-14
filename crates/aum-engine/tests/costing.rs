//! Costing aggregated usage, per model.
//!
//! The mistake this guards against is arithmetically invisible: sum a day's
//! tokens, apply one rate, get a number that looks right and matches no actual
//! price. It only shows up when a day spans models whose rates differ — which,
//! on any machine running both Claude Code and Codex, is most days.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use aum_db::usage::{Filter, Totals, by_day_model};
use aum_engine::prices::{cost_by_bucket, cost_of_slices, save_price};
use aum_pricing::Rates;
use rust_decimal::Decimal;
use std::str::FromStr as _;

fn rates(input: &str, output: &str) -> Rates {
    let d = |s: &str| Decimal::from_str(s).unwrap();
    Rates {
        input_per_mtok: d(input),
        output_per_mtok: d(output),
        cache_read_per_mtok: d("0"),
        cache_write_5m_per_mtok: d("0"),
        cache_write_1h_per_mtok: d("0"),
    }
}

async fn insert(db: &aum_db::Database, at: &str, model: &str, input: u64, output: u64) {
    aum_db::repo::upsert_usage(
        db.writer(),
        &aum_db::repo::UsageRecord {
            adapter_id: "claude_code".to_owned(),
            session_id: "s".to_owned(),
            dedup_key: format!("{at}-{model}-{}", uuid::Uuid::new_v4()),
            model_id: Some(model.to_owned()),
            occurred_at: chrono::DateTime::parse_from_rfc3339(at)
                .unwrap()
                .with_timezone(&chrono::Utc),
            measurement_source: "provider_exact".to_owned(),
            request_kind: "turn".to_owned(),
            usage: aum_domain::TokenUsage::from_bands(aum_contract::TokenBands {
                input_fresh: input,
                cache_read: 0,
                cache_write_5m: 0,
                cache_write_1h: 0,
                cache_write_unspecified: 0,
                output_total: output,
                reasoning: None,
                unclassified: 0,
            }),
            is_sidechain: false,
            agent_id: None,
            agent_type: None,
            raw_json: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_day_spanning_two_models_costs_more_than_either_single_rate_implies() {
    // One million input tokens on each of two models: $5/Mtok and $2/Mtok. The
    // right answer is $7. Blending — two million tokens at either rate — gives
    // $10 or $4, and both look entirely reasonable on a screen.
    let db = aum_db::open_in_memory().await.unwrap();
    save_price(&db, "expensive", &rates("5.00", "0"), None)
        .await
        .unwrap();
    save_price(&db, "cheap", &rates("2.00", "0"), None)
        .await
        .unwrap();

    insert(&db, "2026-08-13T09:00:00Z", "expensive", 1_000_000, 0).await;
    insert(&db, "2026-08-13T10:00:00Z", "cheap", 1_000_000, 0).await;

    let table = aum_engine::prices::load_table(&db).await.unwrap();
    let slices = by_day_model(db.reader(), &Filter::all()).await.unwrap();

    let cost = cost_of_slices(slices.iter().map(|s| (&s.model_id, &s.totals)), &table);
    let got = cost.value.unwrap().amount();

    assert_eq!(got, Decimal::from_str("7").unwrap());
    assert_ne!(
        got,
        Decimal::from_str("10").unwrap(),
        "blended at the higher rate"
    );
    assert_ne!(
        got,
        Decimal::from_str("4").unwrap(),
        "blended at the lower rate"
    );
}

#[tokio::test]
async fn one_unpriced_model_makes_the_total_a_floor_that_says_so() {
    // The priced half is real and worth showing; what must not happen is
    // presenting it as the whole. It comes back as a floor carrying how many
    // models were missing, so the number reads as "at least this".
    let db = aum_db::open_in_memory().await.unwrap();
    save_price(&db, "priced", &rates("5.00", "0"), None)
        .await
        .unwrap();

    insert(&db, "2026-08-13T09:00:00Z", "priced", 1_000_000, 0).await;
    insert(&db, "2026-08-13T10:00:00Z", "unpriced", 1_000_000, 0).await;

    let table = aum_engine::prices::load_table(&db).await.unwrap();
    let slices = by_day_model(db.reader(), &Filter::all()).await.unwrap();
    let cost = cost_of_slices(slices.iter().map(|s| (&s.model_id, &s.totals)), &table);

    // The half that could be priced: 1 Mtok at $5.
    assert_eq!(
        cost.value.unwrap().amount(),
        Decimal::from_str("5").unwrap()
    );
    assert_eq!(cost.display_kind(), aum_contract::DisplayKind::Partial);
    let why = format!("{:?}", cost.accuracy);
    assert!(why.contains("no price for their model"), "{why}");
}

#[tokio::test]
async fn each_bucket_is_costed_from_its_own_models() {
    let db = aum_db::open_in_memory().await.unwrap();
    save_price(&db, "expensive", &rates("5.00", "0"), None)
        .await
        .unwrap();
    save_price(&db, "cheap", &rates("2.00", "0"), None)
        .await
        .unwrap();

    insert(&db, "2026-08-13T09:00:00Z", "expensive", 1_000_000, 0).await;
    insert(&db, "2026-08-14T09:00:00Z", "cheap", 1_000_000, 0).await;

    let table = aum_engine::prices::load_table(&db).await.unwrap();
    let slices = by_day_model(db.reader(), &Filter::all()).await.unwrap();
    let per_day = cost_by_bucket(&slices, &table);

    assert_eq!(per_day.len(), 2);
    assert_eq!(per_day[0].0, "2026-08-13");
    assert_eq!(
        per_day[0].1.value.unwrap().amount(),
        Decimal::from_str("5").unwrap()
    );
    assert_eq!(
        per_day[1].1.value.unwrap().amount(),
        Decimal::from_str("2").unwrap()
    );
}

#[tokio::test]
async fn a_day_with_nothing_recorded_is_unavailable_rather_than_free() {
    // "$0.00" would assert the day cost nothing. It asserts nothing at all —
    // there is no usage to price, which is a different statement.
    let db = aum_db::open_in_memory().await.unwrap();
    let table = aum_engine::prices::load_table(&db).await.unwrap();
    let slices: Vec<aum_db::usage::Slice> = Vec::new();
    let cost = cost_of_slices(slices.iter().map(|s| (&s.model_id, &s.totals)), &table);
    assert_eq!(cost.value, None);
    assert_eq!(cost.display_kind(), aum_contract::DisplayKind::Unavailable);
}

#[tokio::test]
async fn a_totals_slice_keeps_its_bands_disjoint_through_costing() {
    // Regression shape: input_side must never double-count cache reads, which
    // is what makes the two providers' figures comparable at all.
    let t = Totals {
        input_fresh: 2,
        cache_read: 19_712,
        cache_write_1h: 6_993,
        output_total: 50,
        ..Totals::default()
    };
    assert_eq!(t.input_side(), 2 + 19_712 + 6_993);
    assert_eq!(t.grand_total(), 2 + 19_712 + 6_993 + 50);
}
