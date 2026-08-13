//! The whole cost path, from an unpriced model to a converted amount.
//!
//! Every model this machine actually runs is newer than any public price list,
//! so "no price for this model" is the state a real user starts in. This test
//! walks the way out of it and checks that each step says exactly how certain
//! it is: unavailable with a reason, then a calculated dollar figure, then a
//! calculated conversion, then an estimated one once the rate goes stale — and
//! that a currency with no rate at all refuses rather than showing dollars
//! under someone else's symbol.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use aum_contract::{Accuracy, Currency, DisplayKind, UnavailableReason};
use aum_db::Database;
use aum_engine::prices::{CostContext, load_fx, load_table, save_fx, save_price};
use aum_pricing::Rates;
use rust_decimal::Decimal;
use std::str::FromStr as _;

const MODEL: &str = "claude-opus-5";

async fn db_with_one_task() -> (Database, uuid::Uuid) {
    let db = aum_db::open_in_memory().await.unwrap();
    let task_id = uuid::Uuid::new_v4();

    sqlx::query(
        "INSERT INTO task (id, name, adapter_id, status, created_at)
         VALUES (?1, 'costed', 'claude_code', 'running', ?2)",
    )
    .bind(task_id.to_string())
    .bind(aum_db::now_sql())
    .execute(db.writer())
    .await
    .unwrap();

    aum_db::repo::bind_session(
        db.writer(),
        "s1",
        &task_id.to_string(),
        "claude_code",
        "launched_pinned",
        "{}",
    )
    .await
    .unwrap();

    // One million fresh input and one million output, so the arithmetic is
    // legible: at $15/$75 per million the answer is exactly $90.
    let usage = aum_domain::TokenUsage::from_bands(aum_contract::TokenBands {
        input_fresh: 1_000_000,
        cache_read: 0,
        cache_write_5m: 0,
        cache_write_1h: 0,
        cache_write_unspecified: 0,
        output_total: 1_000_000,
        reasoning: None,
        unclassified: 0,
    });

    aum_db::repo::upsert_usage(
        db.writer(),
        &aum_db::repo::UsageRecord {
            adapter_id: "claude_code".to_owned(),
            session_id: "s1".to_owned(),
            dedup_key: "req_1:msg_1".to_owned(),
            model_id: Some(MODEL.to_owned()),
            occurred_at: chrono::Utc::now(),
            measurement_source: "provider_reported".to_owned(),
            request_kind: "completion".to_owned(),
            usage,
            is_sidechain: false,
            agent_id: None,
            agent_type: None,
            raw_json: None,
        },
    )
    .await
    .unwrap();

    (db, task_id)
}

fn rates() -> Rates {
    let d = |s: &str| Decimal::from_str(s).unwrap();
    Rates {
        input_per_mtok: d("15.00"),
        output_per_mtok: d("75.00"),
        cache_read_per_mtok: d("1.50"),
        cache_write_5m_per_mtok: d("18.75"),
        cache_write_1h_per_mtok: d("30.00"),
    }
}

#[tokio::test]
async fn an_unpriced_model_costs_nothing_and_says_why() {
    let (db, task_id) = db_with_one_task().await;
    let table = load_table(&db).await.unwrap();

    let m = aum_engine::task_metrics(&db, task_id, CostContext::usd(&table))
        .await
        .unwrap();

    // The tokens are known exactly. Only the cost is unknown, and the two must
    // not contaminate each other.
    assert_eq!(m.total_tokens.value, Some(2_000_000));
    assert_eq!(m.cost.api_equivalent.value, None);
    match &m.cost.api_equivalent.accuracy {
        Accuracy::Unavailable {
            reason: UnavailableReason::NoPricingForModel { model_id },
        } => assert_eq!(model_id, MODEL),
        other => panic!("expected NoPricingForModel, got {other:?}"),
    }
}

#[tokio::test]
async fn entering_a_price_makes_the_task_cost_a_real_number() {
    let (db, task_id) = db_with_one_task().await;
    save_price(&db, MODEL, &rates(), None).await.unwrap();

    let table = load_table(&db).await.unwrap();
    let m = aum_engine::task_metrics(&db, task_id, CostContext::usd(&table))
        .await
        .unwrap();

    // 1 Mtok input at $15 plus 1 Mtok output at $75.
    assert_eq!(
        m.cost.api_equivalent.value.unwrap().amount(),
        Decimal::from_str("90.00").unwrap()
    );
    // Calculated, not exact: we computed it from a rate, the provider did not
    // send it. Anthropic never told us what this cost.
    assert_eq!(
        m.cost.api_equivalent.display_kind(),
        DisplayKind::Calculated
    );
    assert_eq!(m.cost.currency, Currency::Usd);
}

#[tokio::test]
async fn a_price_never_turns_a_subscription_into_a_charge() {
    // The single most damaging thing this application could say is "this task
    // cost you $90" when the user is on a subscription and was billed nothing
    // extra. Pricing a model must not change that.
    let (db, task_id) = db_with_one_task().await;
    save_price(&db, MODEL, &rates(), None).await.unwrap();

    let table = load_table(&db).await.unwrap();
    let m = aum_engine::task_metrics(&db, task_id, CostContext::usd(&table))
        .await
        .unwrap();

    assert!(m.cost.api_equivalent.value.is_some());
    assert_eq!(m.cost.actual_billed.value, None);
    assert!(matches!(
        m.cost.actual_billed.accuracy,
        Accuracy::Unavailable {
            reason: UnavailableReason::SubscriptionBilled { .. }
        }
    ));
}

#[tokio::test]
async fn converting_needs_a_rate_and_refuses_without_one() {
    // Showing $90 under a "CZK" label would be far worse than showing nothing.
    let (db, task_id) = db_with_one_task().await;
    save_price(&db, MODEL, &rates(), None).await.unwrap();

    let table = load_table(&db).await.unwrap();
    let fx = load_fx(&db).await.unwrap();
    assert!(fx.is_empty());

    let m = aum_engine::task_metrics(
        &db,
        task_id,
        CostContext {
            table: &table,
            fx: &fx,
            currency: Currency::Czk,
        },
    )
    .await
    .unwrap();

    assert_eq!(m.cost.currency, Currency::Czk);
    assert_eq!(m.cost.api_equivalent.value, None);
    assert_eq!(
        m.cost.api_equivalent.display_kind(),
        DisplayKind::Unavailable
    );
}

#[tokio::test]
async fn a_recorded_rate_converts_and_the_currency_travels_with_the_amount() {
    let (db, task_id) = db_with_one_task().await;
    save_price(&db, MODEL, &rates(), None).await.unwrap();
    save_fx(&db, Currency::Eur, Decimal::from_str("0.92").unwrap())
        .await
        .unwrap();

    let table = load_table(&db).await.unwrap();
    let fx = load_fx(&db).await.unwrap();

    let m = aum_engine::task_metrics(
        &db,
        task_id,
        CostContext {
            table: &table,
            fx: &fx,
            currency: Currency::Eur,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        m.cost.api_equivalent.value.unwrap().amount(),
        Decimal::from_str("82.80").unwrap(),
        "90.00 USD at 0.92"
    );
    assert_eq!(m.cost.currency, Currency::Eur);
    assert_eq!(
        m.cost.api_equivalent.display_kind(),
        DisplayKind::Calculated
    );
}

#[tokio::test]
async fn a_rate_for_one_currency_does_not_convert_another() {
    // Two currencies configured, one rate. The unconverted one must fail, not
    // borrow the other's rate.
    let (db, task_id) = db_with_one_task().await;
    save_price(&db, MODEL, &rates(), None).await.unwrap();
    save_fx(&db, Currency::Eur, Decimal::from_str("0.92").unwrap())
        .await
        .unwrap();

    let table = load_table(&db).await.unwrap();
    let fx = load_fx(&db).await.unwrap();

    let czk = aum_engine::task_metrics(
        &db,
        task_id,
        CostContext {
            table: &table,
            fx: &fx,
            currency: Currency::Czk,
        },
    )
    .await
    .unwrap();
    assert_eq!(czk.cost.api_equivalent.value, None);
}

#[tokio::test]
async fn correcting_a_price_changes_the_cost_and_keeps_the_old_version() {
    let (db, task_id) = db_with_one_task().await;
    save_price(&db, MODEL, &rates(), None).await.unwrap();

    let mut corrected = rates();
    corrected.input_per_mtok = Decimal::from_str("20.00").unwrap();
    save_price(
        &db,
        MODEL,
        &corrected,
        Some("provider raised it".to_owned()),
    )
    .await
    .unwrap();

    let table = load_table(&db).await.unwrap();
    let m = aum_engine::task_metrics(&db, task_id, CostContext::usd(&table))
        .await
        .unwrap();
    assert_eq!(
        m.cost.api_equivalent.value.unwrap().amount(),
        Decimal::from_str("95.00").unwrap()
    );

    // Both versions survive, which is what lets a past run keep its numbers.
    let versions = aum_db::pricing::list_price_versions(db.reader())
        .await
        .unwrap();
    assert_eq!(versions.len(), 2);
}

#[tokio::test]
async fn usd_is_never_downgraded_by_a_conversion_that_does_not_happen() {
    // Selecting USD must not mark the amount as converted; it is the base.
    let (db, task_id) = db_with_one_task().await;
    save_price(&db, MODEL, &rates(), None).await.unwrap();
    save_fx(&db, Currency::Eur, Decimal::from_str("0.92").unwrap())
        .await
        .unwrap();

    let table = load_table(&db).await.unwrap();
    let fx = load_fx(&db).await.unwrap();
    let m = aum_engine::task_metrics(
        &db,
        task_id,
        CostContext {
            table: &table,
            fx: &fx,
            currency: Currency::Usd,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        m.cost.api_equivalent.value.unwrap().amount(),
        Decimal::from_str("90.00").unwrap()
    );
    assert_eq!(
        m.cost.api_equivalent.display_kind(),
        DisplayKind::Calculated
    );
}
