//! The days `aum` reports are the user's days, end to end.
//!
//! This runs the real binary with its zone set, because the two halves of what
//! it guards live in different places. The command line cuts a range at local
//! midnight with chrono, and SQLite labels the buckets inside it through the C
//! library. Each half was right on its own while they disagreed: UTC buckets in
//! a local range, so `aum daily --today` could print a row dated yesterday.

#![allow(clippy::unwrap_used, clippy::expect_used)]

/// Central Europe, as a POSIX rule so the test needs no tz database: UTC+1, and
/// UTC+2 from the last Sunday in March to the last in October.
const PRAGUE: &str = "CET-1CEST,M3.5.0,M10.5.0/3";

async fn insert(db: &aum_db::Database, at: &str) {
    aum_db::repo::upsert_usage(
        db.writer(),
        &aum_db::repo::UsageRecord {
            adapter_id: "claude_code".to_owned(),
            session_id: "s".to_owned(),
            dedup_key: at.to_owned(),
            model_id: Some("m".to_owned()),
            occurred_at: chrono::DateTime::parse_from_rfc3339(at)
                .unwrap()
                .with_timezone(&chrono::Utc),
            measurement_source: "provider_exact".to_owned(),
            request_kind: "turn".to_owned(),
            usage: aum_domain::TokenUsage::from_bands(aum_contract::TokenBands {
                input_fresh: 1,
                cache_read: 0,
                cache_write_5m: 0,
                cache_write_1h: 0,
                cache_write_unspecified: 0,
                output_total: 0,
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

/// A database holding one request at each of these instants.
async fn recorded(at: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture.sqlite3");
    let db = aum_db::open_at(&path).await.unwrap();
    for a in at {
        insert(&db, a).await;
    }
    db.close().await;
    (dir, path)
}

/// What `aum` prints for these arguments, run in Prague.
fn aum_in_prague(db: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_aum"))
        .args(args)
        .args(["--no-sync", "--db"])
        .arg(db)
        .env("TZ", PRAGUE)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Each bucket's label and request count, from a report's JSON.
fn buckets(json: &str) -> Vec<(String, i64)> {
    let report: serde_json::Value = serde_json::from_str(json).unwrap();
    report["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["at"].as_str().unwrap().to_owned(),
                b["totals"]["requests"].as_i64().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_day_in_prague_is_reported_as_pragues_day() {
    // 23:30 on the 12th, 00:30 and 23:30 on the 13th, and 00:30 on the 14th,
    // in summer time — two hours ahead of the UTC written here.
    let (_dir, db) = recorded(&[
        "2026-08-12T21:30:00Z",
        "2026-08-12T22:30:00Z",
        "2026-08-13T21:30:00Z",
        "2026-08-13T22:30:00Z",
    ])
    .await;

    // The same local midnights `--today` cuts at, on a date that holds still.
    let day = ["--since", "2026-08-13", "--until", "2026-08-14", "--json"];

    let daily = aum_in_prague(&db, &[&["daily"][..], &day].concat());
    assert_eq!(buckets(&daily), [("2026-08-13".to_owned(), 2)]);

    let hourly = aum_in_prague(&db, &[&["hourly"][..], &day].concat());
    assert_eq!(
        buckets(&hourly),
        [
            ("2026-08-13T00".to_owned(), 1),
            ("2026-08-13T23".to_owned(), 1)
        ]
    );
}

#[tokio::test]
async fn a_session_was_last_active_at_pragues_time() {
    // Beside the hourly rows, which put this request at 10.
    let (_dir, db) = recorded(&["2026-08-13T08:30:00Z"]).await;
    let table = aum_in_prague(&db, &["sessions"]);
    assert!(table.contains("2026-08-13 10:30"), "{table}");
}
