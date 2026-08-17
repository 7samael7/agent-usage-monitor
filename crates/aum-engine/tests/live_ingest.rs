//! The ingest loop, as something that keeps running.
//!
//! `Engine::pass` had tests. `Engine::run` — the loop that makes the numbers
//! move while you watch — had none, and for a while nothing called it: the
//! interactive view re-read the database every two seconds, printed a fresh
//! timestamp, and never once looked at the transcripts. Every number was
//! frozen at whatever the single startup pass had found, and the clock beside
//! them said otherwise.
//!
//! These tests are about the loop being *running*, which is the part that was
//! missing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::time::Duration;

use aum_engine::Engine;

/// One assistant turn, shaped like the real thing: the fields the parser reads
/// and nothing else.
fn turn(n: u32, output_tokens: u32) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"1111{n:04}-0000-4000-8000-000000000000",
        "sessionId":"aaaaaaaa-0000-4000-8000-000000000001","requestId":"req_{n}",
        "timestamp":"2026-08-17T09:00:0{n}.000Z",
        "message":{{"id":"msg_{n}","type":"message","role":"assistant",
        "model":"claude-opus-5","stop_reason":"end_turn",
        "content":[{{"type":"text","text":"xxxx"}}],
        "usage":{{"input_tokens":10,"output_tokens":{output_tokens},
        "cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
    )
    .replace('\n', "")
}

fn transcript_in(home: &Path) -> std::path::PathBuf {
    let dir = home.join(".claude").join("projects").join("-p");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("aaaaaaaa-0000-4000-8000-000000000001.jsonl")
}

async fn recorded(db: &aum_db::Database) -> i64 {
    aum_db::usage::totals(db.reader(), &aum_db::usage::Filter::default())
        .await
        .unwrap()
        .requests
}

/// Poll until `f` holds, or give up. Polling rather than sleeping a fixed
/// interval keeps the test quick when it passes and honest when it fails.
async fn within<F, Fut>(limit: Duration, mut f: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        if f().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transcript_that_grows_while_the_loop_runs_is_picked_up() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let transcript = transcript_in(home.path());
    std::fs::write(&transcript, turn(1, 1000) + "\n").unwrap();

    let db = aum_db::open(data.path()).await.unwrap();
    let engine = Engine::new(db.clone(), home.path());
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(engine.run(stop_rx));

    assert!(
        within(Duration::from_secs(10), || async {
            recorded(&db).await == 1
        })
        .await,
        "the first pass never recorded the turn that was already there"
    );

    // The part that matters: the file grows *after* the loop has started and
    // already decided it had read everything.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    std::io::Write::write_all(&mut f, (turn(2, 7000) + "\n").as_bytes()).unwrap();
    drop(f);

    assert!(
        within(Duration::from_secs(10), || async {
            recorded(&db).await == 2
        })
        .await,
        "a turn appended while the loop was running was never read"
    );

    stop.send(true).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_loop_stops_when_it_is_told_to() {
    // Otherwise quitting the interface would leave a task reading the user's
    // transcripts for as long as the process lived.
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(transcript_in(home.path()), turn(1, 5) + "\n").unwrap();

    let db = aum_db::open(data.path()).await.unwrap();
    let engine = Engine::new(db, home.path());
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(engine.run(stop_rx));

    tokio::time::sleep(Duration::from_millis(200)).await;
    stop.send(true).unwrap();

    let ended = tokio::time::timeout(Duration::from_secs(5), task).await;
    assert!(ended.is_ok(), "the loop ignored the stop signal");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wake_cuts_the_idle_wait_short() {
    // What `r` does. Without it the key would have to build a second engine,
    // whose scan cache starts empty — re-reading every file on the machine to
    // discover that nothing had changed.
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let transcript = transcript_in(home.path());
    std::fs::write(&transcript, turn(1, 10) + "\n").unwrap();

    let db = aum_db::open(data.path()).await.unwrap();
    let engine = Engine::new(db.clone(), home.path());
    let state = engine.state_handle();
    let wake = engine.wake_handle();
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(engine.run(stop_rx));

    assert!(
        within(Duration::from_secs(10), || async {
            state.read().await.passes >= 1
        })
        .await,
        "no pass completed"
    );

    let before = state.read().await.passes;
    wake.notify_one();

    // The idle interval is two seconds; a wake should beat it comfortably.
    // The margin is what keeps this from being a stopwatch test.
    assert!(
        within(Duration::from_millis(900), || async {
            state.read().await.passes > before
        })
        .await,
        "a wake did not bring the next pass forward"
    );

    stop.send(true).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_completed_pass_is_dated() {
    // The status bar reports this age. If it were never set, "checked just now"
    // would be a guess.
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(transcript_in(home.path()), turn(1, 10) + "\n").unwrap();

    let db = aum_db::open(data.path()).await.unwrap();
    let engine = Engine::new(db, home.path());
    let state = engine.state_handle();
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(engine.run(stop_rx));

    assert!(
        within(Duration::from_secs(10), || async {
            state.read().await.last_pass_at.is_some()
        })
        .await,
        "a pass completed without recording when"
    );

    stop.send(true).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}
