//! Twenty tasks at once, under conditions designed to make attribution fail.
//!
//! Attribution here is a single indexed equality lookup from `session_id` to
//! `task_id`, and `task_binding.session_id` is a PRIMARY KEY, so a session
//! belongs to at most one task by construction. This test exists to prove that
//! the construction actually holds end to end, under every condition that would
//! break a heuristic implementation:
//!
//! * **All twenty writers share one working directory.** Any cwd-based
//!   attribution collapses all twenty into one task and this fails immediately.
//! * **All start in the same millisecond**, so nothing can be resolved by
//!   "whichever task started most recently".
//! * **Two sessions live in the same project directory**, so per-directory
//!   attribution is not available either.
//! * **Five emit subagent transcripts** in a nested `subagents/` directory.
//!   Those carry the *parent* session id and must land in the parent's task —
//!   they are 15–37% of all tokens, so silently dropping them looks like a
//!   plausible total.
//! * **Three emit the fan-out pattern**, one response written as many JSONL
//!   lines each repeating the whole usage object. Summing lines over-counts
//!   those tasks by 2–3x.
//! * **A twenty-first writer is registered to no task at all** and must land
//!   entirely in Unattributed rather than being adopted by a neighbour.
//!
//! The assertion is **set equality per task**, never totals. Two symmetric
//! cross-attributions — task A taking one of B's requests while B takes one of
//! A's — cancel exactly in a sum, so a totals check would pass while every
//! per-task number on screen was wrong.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

const TASKS: usize = 20;
/// Every writer claims this. It is the trap.
const SHARED_CWD: &str = "/Users/someone/shared-project";
/// Every writer claims this instant.
const SAME_MS: &str = "2026-08-13T09:00:00.000Z";

/// One assistant line, as Claude Code writes them.
fn line(session: &str, request: &str, message: &str, output: u64) -> String {
    format!(
        r#"{{"type":"assistant","sessionId":"{session}","cwd":"{SHARED_CWD}","timestamp":"{SAME_MS}","requestId":"{request}","message":{{"id":"{message}","model":"claude-opus-5","usage":{{"input_tokens":10,"output_tokens":{output},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
    )
}

/// The requests one session is supposed to end up owning.
struct Expected {
    session_id: String,
    keys: BTreeSet<String>,
    output_tokens: u64,
    /// Files this session wrote, so the fixture can be checked for the traps it
    /// claims to contain.
    files: Vec<std::path::PathBuf>,
}

/// Fail if the corpus is not actually adversarial.
///
/// A test that passes because its fixture is trivial is worse than no test: it
/// reports a guarantee nobody is checking. These assertions are about the
/// *input*, and they would fire the moment an edit made the corpus easy —
/// before the interesting assertions had a chance to pass vacuously.
fn assert_corpus_is_adversarial(expected: &[Expected]) {
    let mut lines = 0_usize;
    let mut fanned_out = 0_usize;
    let mut with_subagents = 0_usize;

    for e in expected {
        let mut session_lines = 0_usize;
        for path in &e.files {
            let body = std::fs::read_to_string(path).unwrap();
            session_lines += body.lines().filter(|l| !l.trim().is_empty()).count();
            if path.to_string_lossy().contains("subagents") {
                with_subagents += 1;
            }
        }
        if session_lines > e.keys.len() {
            fanned_out += 1;
        }
        lines += session_lines;
    }

    let requests: usize = expected.iter().map(|e| e.keys.len()).sum();
    assert!(
        fanned_out >= 3,
        "at least three sessions must write one response as many lines; got {fanned_out}"
    );
    assert!(
        with_subagents >= 5,
        "at least five sessions must emit subagent transcripts; got {with_subagents}"
    );
    let ids: Vec<&str> = expected.iter().map(|e| e.session_id.as_str()).collect();
    assert!(
        ids.iter()
            .any(|a| ids.iter().any(|b| b != a && b.starts_with(*a))),
        "no session id is a prefix of another, so nothing here would catch an attribution \
         lookup that matched on a prefix instead of on equality"
    );
    // The number that makes the dedup assertions meaningful: counting lines
    // instead of requests over-counts by this much.
    let inflation = lines as f64 / requests as f64;
    assert!(
        inflation > 1.5,
        "summing lines would only over-count by {inflation:.2}x, which is too gentle to catch a \
         broken dedup — the fixture has stopped being adversarial"
    );
}

/// Lay out a home directory for `n` sessions plus one rogue, and return what
/// each session should own.
fn write_corpus(home: &std::path::Path, n: usize) -> Vec<Expected> {
    let projects = home.join(".claude").join("projects");
    let mut expected = Vec::new();

    for i in 0..=n {
        // One id is deliberately a strict prefix of ten others. Real session
        // ids are UUIDs so this does not arise by accident, but it is precisely
        // what an attribution lookup written with LIKE instead of equality
        // would get wrong, and the PRIMARY KEY design claims to exclude it.
        let session = if i == 1 {
            "session-1".to_owned()
        } else {
            format!("session-{i:02}")
        };
        // Two sessions deliberately share a project directory, so nothing can be
        // attributed by which folder a file sits in.
        let project = if i < 2 {
            projects.join("shared-project")
        } else {
            projects.join(format!("project-{i:02}"))
        };
        std::fs::create_dir_all(&project).unwrap();

        let mut keys = BTreeSet::new();
        let mut body = String::new();
        let mut output_tokens = 0_u64;
        let mut files = Vec::new();

        for r in 0..5 {
            let request = format!("req_{i:02}_{r}");
            let message = format!("msg_{i:02}_{r}");
            // Distinct per session, so a stolen request shows up in the total
            // as well as in the set.
            let output = 100 + (i as u64) * 10 + r as u64;

            // Three sessions write each response as eight lines, every one
            // repeating the whole usage object. Summing lines triples them.
            let repeats = if i % 7 == 3 { 8 } else { 1 };
            for _ in 0..repeats {
                body.push_str(&line(&session, &request, &message, output));
                body.push('\n');
            }

            keys.insert(format!("{request}:{message}"));
            output_tokens += output;
        }

        let main_file = project.join(format!("{session}.jsonl"));
        std::fs::write(&main_file, &body).unwrap();
        files.push(main_file);

        // Five sessions also emit subagent transcripts. These carry the parent
        // session id, so they belong to the parent's task.
        if i % 4 == 0 {
            let sub = project.join(&session).join("subagents");
            std::fs::create_dir_all(&sub).unwrap();
            let mut sub_body = String::new();
            for r in 0..3 {
                let request = format!("sub_{i:02}_{r}");
                let message = format!("submsg_{i:02}_{r}");
                let output = 500 + r as u64;
                sub_body.push_str(&line(&session, &request, &message, output));
                sub_body.push('\n');
                keys.insert(format!("{request}:{message}"));
                output_tokens += output;
            }
            let sub_file = sub.join(format!("agent-{i:02}.jsonl"));
            std::fs::write(&sub_file, sub_body).unwrap();
            files.push(sub_file);
        }

        expected.push(Expected {
            session_id: session,
            keys,
            output_tokens,
            files,
        });
    }

    expected
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn twenty_concurrent_tasks_never_take_each_others_requests() {
    let home = tempfile::tempdir().unwrap();
    let db = aum_db::open_in_memory().await.unwrap();

    // Index n is the rogue: a real writer, deliberately bound to nothing.
    let expected = write_corpus(home.path(), TASKS);
    assert_corpus_is_adversarial(&expected);

    let mut task_of_session = BTreeMap::new();
    for e in expected.iter().take(TASKS) {
        let task_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO task (id, name, adapter_id, status, created_at)
             VALUES (?1, ?2, 'claude_code', 'running', ?3)",
        )
        .bind(&task_id)
        .bind(&e.session_id)
        // Identical creation instants: nothing can be resolved by recency.
        .bind(SAME_MS)
        .execute(db.writer())
        .await
        .unwrap();

        aum_db::repo::bind_session(
            db.writer(),
            &e.session_id,
            &task_id,
            "claude_code",
            "launched_pinned",
            "{}",
        )
        .await
        .unwrap();

        task_of_session.insert(e.session_id.clone(), task_id);
    }

    // Read the tree repeatedly and concurrently. Ingest must be idempotent, so
    // overlapping passes over the same files converge on the same answer rather
    // than double-counting.
    let engine = std::sync::Arc::new(aum_engine::Engine::new(db.clone(), home.path()));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let engine = std::sync::Arc::clone(&engine);
        handles.push(tokio::spawn(async move { engine.pass().await }));
    }
    for h in handles {
        h.await.unwrap();
    }

    // ── Every task owns exactly its own requests, and nothing else ──────────

    for e in expected.iter().take(TASKS) {
        let task_id = task_of_session.get(&e.session_id).unwrap();

        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT dedup_key FROM ai_request WHERE task_id = ?1")
                .bind(task_id)
                .fetch_all(db.reader())
                .await
                .unwrap();
        let got: BTreeSet<String> = rows.into_iter().map(|(k,)| k).collect();

        assert_eq!(
            got, e.keys,
            "task for {} owns the wrong set of requests",
            e.session_id
        );

        // The set being right is the strong claim; the total being right proves
        // the fan-out lines collapsed rather than summing.
        let (output,): (i64,) = sqlx::query_as(
            "SELECT COALESCE(SUM(output_total), 0) FROM ai_request WHERE task_id = ?1",
        )
        .bind(task_id)
        .fetch_one(db.reader())
        .await
        .unwrap();
        assert_eq!(
            u64::try_from(output).unwrap(),
            e.output_tokens,
            "{} has the right requests but the wrong total, so a repeated line was counted twice",
            e.session_id
        );
    }

    // ── The unregistered writer is recorded, and attributed to nobody ───────

    let rogue = expected.last().unwrap();
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT dedup_key, task_id FROM ai_request WHERE session_id = ?1")
            .bind(&rogue.session_id)
            .fetch_all(db.reader())
            .await
            .unwrap();

    assert_eq!(
        rows.len(),
        rogue.keys.len(),
        "the rogue session's usage must still be recorded — declining to attribute is not \
         declining to observe"
    );
    for (key, task_id) in rows {
        assert_eq!(
            task_id, None,
            "{key} was adopted by a task despite no binding existing"
        );
    }

    // ── Conservation: everything observed is either attributed or not ───────

    let (total,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ai_request")
        .fetch_one(db.reader())
        .await
        .unwrap();
    let (attributed,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM ai_request WHERE task_id IS NOT NULL")
            .fetch_one(db.reader())
            .await
            .unwrap();
    let unattributed = aum_db::repo::unattributed_count(db.reader()).await.unwrap();

    assert_eq!(
        attributed + unattributed,
        total,
        "every observation must be in exactly one of the two buckets"
    );
    let expected_total: usize = expected.iter().map(|e| e.keys.len()).sum();
    assert_eq!(usize::try_from(total).unwrap(), expected_total);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_round_of_writes_lands_in_the_same_tasks() {
    // The restart guarantee, under concurrency: appending to every transcript
    // and reading again must extend each task rather than redistribute anything.
    let home = tempfile::tempdir().unwrap();
    let db = aum_db::open_in_memory().await.unwrap();
    let expected = write_corpus(home.path(), 4);

    let mut task_of_session = BTreeMap::new();
    for e in expected.iter().take(4) {
        let task_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO task (id, name, adapter_id, status, created_at)
             VALUES (?1, ?2, 'claude_code', 'running', ?3)",
        )
        .bind(&task_id)
        .bind(&e.session_id)
        .bind(SAME_MS)
        .execute(db.writer())
        .await
        .unwrap();
        aum_db::repo::bind_session(
            db.writer(),
            &e.session_id,
            &task_id,
            "claude_code",
            "launched_pinned",
            "{}",
        )
        .await
        .unwrap();
        task_of_session.insert(e.session_id.clone(), task_id);
    }

    let engine = aum_engine::Engine::new(db.clone(), home.path());
    engine.pass().await;

    // Append one more request to every session, in the same shared cwd.
    let projects = home.path().join(".claude").join("projects");
    for (i, e) in expected.iter().take(4).enumerate() {
        let project = if i < 2 {
            projects.join("shared-project")
        } else {
            projects.join(format!("project-{i:02}"))
        };
        let path = project.join(format!("{}.jsonl", e.session_id));
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str(&line(
            &e.session_id,
            &format!("late_{i:02}"),
            &format!("latemsg_{i:02}"),
            42,
        ));
        body.push('\n');
        std::fs::write(&path, body).unwrap();
    }

    engine.pass().await;

    for (i, e) in expected.iter().take(4).enumerate() {
        let task_id = task_of_session.get(&e.session_id).unwrap();
        let mut want = e.keys.clone();
        want.insert(format!("late_{i:02}:latemsg_{i:02}"));

        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT dedup_key FROM ai_request WHERE task_id = ?1")
                .bind(task_id)
                .fetch_all(db.reader())
                .await
                .unwrap();
        let got: BTreeSet<String> = rows.into_iter().map(|(k,)| k).collect();
        assert_eq!(got, want, "{} drifted across a second pass", e.session_id);
    }
}
