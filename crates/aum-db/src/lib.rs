//! # `aum-db` — local storage
//!
//! One SQLite file, in the OS application-support directory. Nothing leaves the
//! machine.
//!
//! ## Two pools, one writer
//!
//! SQLite tolerates exactly one writer. Rather than discover that under load,
//! the write pool is capped at a single connection and owned by a batching
//! actor; reads get their own pool and never contend with it.
//!
//! ## Why runtime queries rather than the `query!` macros
//!
//! The compile-time-checked macros require either a live `DATABASE_URL` at
//! build time or a committed `.sqlx/` offline cache kept in sync by hand. Both
//! couple `cargo build` to database state, which is a persistent papercut. The
//! same safety is recovered by running every query against a freshly migrated
//! temporary database in tests — same guarantee, no build coupling.

pub mod desktop;
pub mod pricing;
pub mod repo;
pub mod usage;
pub mod writer;

use std::path::Path;
use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Pool, Sqlite};

pub type Db = Pool<Sqlite>;

/// Embedded at compile time, so the sidecar is a self-contained binary with no
/// migration files to ship alongside it.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("could not create the data directory {path}: {source}")]
    DataDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Open (creating if needed) the capture database, and run migrations.
pub async fn open(data_dir: &Path) -> Result<Database, DbError> {
    std::fs::create_dir_all(data_dir).map_err(|source| DbError::DataDir {
        path: data_dir.display().to_string(),
        source,
    })?;
    let path = data_dir.join("capture.sqlite3");
    open_at(&path).await
}

/// Open a database at an exact path. Used by tests with a temporary file.
pub async fn open_at(path: &Path) -> Result<Database, DbError> {
    let options = connect_options(path);

    // A single write connection: SQLite permits one writer, so serializing here
    // is honest about the constraint rather than relying on retry-on-busy.
    let write = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await?;

    MIGRATOR.run(&write).await?;

    let read = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options.read_only(false))
        .await?;

    Ok(Database { write, read })
}

/// An in-memory database, for tests that do not need a file.
pub async fn open_in_memory() -> Result<Database, DbError> {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap_or_default()
        .foreign_keys(true);

    // A shared in-memory database lives only as long as its connections, so the
    // same single connection must serve reads and writes here.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    MIGRATOR.run(&pool).await?;
    Ok(Database {
        write: pool.clone(),
        read: pool,
    })
}

fn connect_options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        // WAL lets readers proceed during a write, which matters because the
        // dashboard queries continuously while ingest is writing.
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        // NORMAL rather than FULL: with WAL this risks losing only the last
        // transaction on an OS crash, and the data is re-derivable by re-reading
        // the transcripts. Paying an fsync per commit for that would be a poor
        // trade in an application that writes continuously.
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(5))
        .pragma("temp_store", "MEMORY")
        .pragma("mmap_size", "268435456")
}

#[derive(Clone)]
pub struct Database {
    write: Db,
    read: Db,
}

impl Database {
    /// The single write connection. Held by the writer actor.
    #[must_use]
    pub fn writer(&self) -> &Db {
        &self.write
    }

    #[must_use]
    pub fn reader(&self) -> &Db {
        &self.read
    }

    /// Flush and close. Called on shutdown, before the process exits.
    pub async fn close(&self) {
        self.read.close().await;
        self.write.close().await;
    }
}

/// Format a timestamp the way every column in this schema expects.
#[must_use]
pub fn to_sql_time(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

#[must_use]
pub fn now_sql() -> String {
    to_sql_time(chrono::Utc::now())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[tokio::test]
    async fn migrations_apply_to_a_fresh_database() {
        let db = open_in_memory().await.unwrap();
        let tables: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .fetch_all(db.reader())
                .await
                .unwrap();
        let names: Vec<&str> = tables.iter().map(|t| t.0.as_str()).collect();

        for expected in [
            "ai_request",
            "task",
            "task_binding",
            "token_usage",
            "cost_calculation",
            "ingest_cursor",
            "ingest_file",
            "pricing_version",
        ] {
            assert!(names.contains(&expected), "missing table {expected}");
        }
    }

    #[tokio::test]
    async fn a_session_cannot_be_bound_to_two_tasks() {
        // The structural anti-cross-attribution guarantee. If this ever stops
        // holding, twenty concurrent tasks can silently contaminate each other.
        let db = open_in_memory().await.unwrap();
        let now = now_sql();

        for id in ["task-a", "task-b"] {
            sqlx::query(
                "INSERT INTO task (id, name, adapter_id, status, created_at)
                 VALUES (?, ?, 'claude_code', 'running', ?)",
            )
            .bind(id)
            .bind(id)
            .bind(&now)
            .execute(db.writer())
            .await
            .unwrap();
        }

        let bind = |task: &'static str| {
            let now = now.clone();
            let pool = db.writer().clone();
            async move {
                sqlx::query(
                    "INSERT INTO task_binding (session_id, task_id, adapter_id, method, evidence, bound_at)
                     VALUES ('session-1', ?, 'claude_code', 'launched_pinned', '{}', ?)",
                )
                .bind(task)
                .bind(now)
                .execute(&pool)
                .await
            }
        };

        assert!(bind("task-a").await.is_ok());
        // The second bind must fail loudly rather than steal the session.
        assert!(bind("task-b").await.is_err());
    }

    #[tokio::test]
    async fn the_same_request_cannot_be_recorded_twice() {
        // One Claude API response is written to as many as 21 JSONL lines. If
        // this uniqueness fails, every total is 2-3x too high.
        let db = open_in_memory().await.unwrap();
        let now = now_sql();

        let insert = |id: &'static str| {
            let now = now.clone();
            let pool = db.writer().clone();
            async move {
                sqlx::query(
                    "INSERT INTO ai_request
                       (id, adapter_id, session_id, dedup_key, occurred_at,
                        measurement_source, request_kind, created_at)
                     VALUES (?, 'claude_code', 'sess-1', 'req_1:msg_1', ?, 'provider_exact', 'turn', ?)",
                )
                .bind(id)
                .bind(&now)
                .bind(&now)
                .execute(&pool)
                .await
            }
        };

        assert!(insert("row-1").await.is_ok());
        assert!(insert("row-2").await.is_err());
    }

    #[tokio::test]
    async fn reasoning_can_be_null_and_is_not_coerced_to_zero() {
        let db = open_in_memory().await.unwrap();
        let now = now_sql();
        sqlx::query(
            "INSERT INTO ai_request
               (id, adapter_id, session_id, dedup_key, occurred_at,
                measurement_source, request_kind, created_at, reasoning)
             VALUES ('r1', 'claude_code', 's1', 'k1', ?, 'provider_exact', 'turn', ?, NULL)",
        )
        .bind(&now)
        .bind(&now)
        .execute(db.writer())
        .await
        .unwrap();

        let (reasoning,): (Option<i64>,) =
            sqlx::query_as("SELECT reasoning FROM ai_request WHERE id = 'r1'")
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(reasoning, None);
    }

    #[tokio::test]
    async fn deleting_a_task_leaves_its_usage_unattributed_rather_than_destroying_it() {
        // Usage is evidence. Removing a task should forget the grouping, not the
        // measurement — otherwise the conservation check silently stops holding.
        let db = open_in_memory().await.unwrap();
        let now = now_sql();

        sqlx::query(
            "INSERT INTO task (id, name, adapter_id, status, created_at)
             VALUES ('t1', 'x', 'claude_code', 'running', ?)",
        )
        .bind(&now)
        .execute(db.writer())
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO ai_request
               (id, adapter_id, session_id, task_id, dedup_key, occurred_at,
                measurement_source, request_kind, created_at, output_total)
             VALUES ('r1', 'claude_code', 's1', 't1', 'k1', ?, 'provider_exact', 'turn', ?, 500)",
        )
        .bind(&now)
        .bind(&now)
        .execute(db.writer())
        .await
        .unwrap();

        sqlx::query("DELETE FROM task WHERE id = 't1'")
            .execute(db.writer())
            .await
            .unwrap();

        let (task_id, output): (Option<String>, i64) =
            sqlx::query_as("SELECT task_id, output_total FROM ai_request WHERE id = 'r1'")
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(task_id, None);
        assert_eq!(output, 500);
    }
}
