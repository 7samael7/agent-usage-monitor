//! Comparison against real storage.
//!
//! The unit tests cover the arithmetic; these cover what happens when a
//! selection meets a database — in particular a stale one, since a comparison
//! selection lives in a URL and URLs outlive the things they point at.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use aum_engine::prices::CostContext;
use aum_pricing::PriceTable;

async fn task(db: &aum_db::Database, name: &str) -> uuid::Uuid {
    let id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO task (id, name, adapter_id, status, created_at)
         VALUES (?1, ?2, 'claude_code', 'completed', ?3)",
    )
    .bind(id.to_string())
    .bind(name)
    .bind(aum_db::now_sql())
    .execute(db.writer())
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn a_task_that_is_not_in_this_database_is_left_out_and_named() {
    // A shared comparison link outlives the machine it was made on. Rendering a
    // row of zeros for a task nobody has would be a confident statement about
    // something that was never measured.
    let db = aum_db::open_in_memory().await.unwrap();
    let real = task(&db, "kept").await;
    let ghost = uuid::Uuid::new_v4();
    let table = PriceTable::seed();

    let c = aum_engine::compare::compare(&db, &[real, ghost], CostContext::usd(&table), false)
        .await
        .unwrap();

    assert_eq!(c.rows.len(), 1, "only the task that exists");
    assert_eq!(c.rows[0].task_id, real);
    assert!(
        c.caveats.iter().any(|s| s.contains(&ghost.to_string())),
        "the missing id must be named, not silently dropped: {:?}",
        c.caveats
    );
}

#[tokio::test]
async fn rows_come_back_in_the_order_they_were_asked_for() {
    // The order is the reader's choice — usually "mine first" — and reordering
    // it would silently rearrange a comparison someone had set up.
    let db = aum_db::open_in_memory().await.unwrap();
    let a = task(&db, "a").await;
    let b = task(&db, "b").await;
    let table = PriceTable::seed();

    let c = aum_engine::compare::compare(&db, &[b, a], CostContext::usd(&table), false)
        .await
        .unwrap();
    assert_eq!(
        c.rows.iter().map(|r| r.task_id).collect::<Vec<_>>(),
        vec![b, a]
    );
}

#[tokio::test]
async fn an_empty_selection_compares_nothing_rather_than_everything() {
    let db = aum_db::open_in_memory().await.unwrap();
    task(&db, "a").await;
    let table = PriceTable::seed();

    let c = aum_engine::compare::compare(&db, &[], CostContext::usd(&table), false)
        .await
        .unwrap();
    assert!(c.rows.is_empty());
    assert!(c.caveats.is_empty());
}
