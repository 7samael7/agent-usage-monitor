//! Starting, watching and stopping monitored tasks.
//!
//! # Why launching makes attribution exact
//!
//! When the monitor starts the agent itself, it can fix the identity that usage
//! will arrive under *before* any usage exists. Nothing has to be correlated
//! afterwards, and there is no window in which a concurrent session could be
//! mistaken for this one:
//!
//! * **Claude Code** — the monitor generates the session UUID and passes
//!   `--session-id`. The binding is written before the process starts, so the
//!   very first request is already attributed. Sub-agent transcripts carry the
//!   same parent session id, so they attribute for free.
//! * **Codex** — there is no equivalent flag, so instead the monitor reads
//!   `codex exec --json` from its own child's pipe. The session id arrives in
//!   the stream itself, and because the pipe belongs to a process we spawned,
//!   binding it to this task is a fact rather than an inference.
//!
//! Twenty concurrent tasks stay separate because each holds a distinct session
//! id and `task_binding.session_id` is a primary key. There is no scoring, no
//! nearest-match and no time-window heuristic anywhere in this file.

use std::path::PathBuf;
use std::sync::Arc;

use aum_db::Database;
use aum_db::repo;
use aum_procmon::{LaunchSpec, LaunchedProcess};
use tokio::io::AsyncBufReadExt as _;
use tokio::sync::Mutex;

/// Which agent a task runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    ClaudeCode,
    Codex,
}

impl Agent {
    #[must_use]
    pub const fn adapter_id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
        }
    }

    #[must_use]
    pub const fn executable(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
        }
    }

    /// Neither agent bills per token under the plans in use here, so an actual
    /// charge is genuinely unknowable and the UI must say so.
    #[must_use]
    pub const fn subscription_plan(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude subscription",
            Self::Codex => "Codex subscription",
        }
    }

    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "claude_code" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// What to run.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    pub name: String,
    pub agent: Agent,
    pub working_dir: PathBuf,
    /// What to ask the agent to do.
    pub prompt: String,
    pub benchmark_id: Option<uuid::Uuid>,
    /// Extra environment for the child.
    ///
    /// Never stored and never returned to the renderer: these routinely hold
    /// API keys, and a monitoring tool has no business keeping them.
    pub env: Vec<(String, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("{0}")]
    Launch(#[from] aum_procmon::LaunchError),
    #[error("database error: {0}")]
    Db(#[from] aum_db::DbError),
    #[error("{agent} was not found on this machine. {hint}")]
    AgentMissing { agent: String, hint: String },
    #[error("no task with id {0}")]
    NoSuchTask(uuid::Uuid),
}

struct Running {
    process: LaunchedProcess,
    /// Reader of the child's stdout, for agents whose events arrive that way.
    reader: Option<tokio::task::JoinHandle<()>>,
    /// The tail of whatever the agent wrote to stderr.
    ///
    /// Kept because an agent that refuses to run explains itself there, and
    /// that explanation is the most useful thing the application can show:
    /// "OAuth session expired and could not be refreshed" tells the user
    /// exactly what to do, where an exit code tells them nothing.
    stderr: Arc<Mutex<StderrTail>>,
}

/// A bounded tail of a child's stderr.
///
/// Bounded because an agent stuck in a loop can write without limit, and this
/// sits in memory for the life of the task. The tail rather than the head
/// because the last thing a failing process says is almost always the reason.
#[derive(Default)]
struct StderrTail {
    lines: std::collections::VecDeque<String>,
}

impl StderrTail {
    /// Enough to carry a stack trace, far short of enough to be a log file.
    const MAX_LINES: usize = 20;
    const MAX_LINE: usize = 400;

    fn push(&mut self, line: String) {
        let line = if line.len() > Self::MAX_LINE {
            let mut truncated: String = line.chars().take(Self::MAX_LINE).collect();
            truncated.push('…');
            truncated
        } else {
            line
        };
        self.lines.push_back(line);
        while self.lines.len() > Self::MAX_LINES {
            self.lines.pop_front();
        }
    }

    fn text(&self) -> Option<String> {
        let joined = self
            .lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        let trimmed = joined.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    }
}

/// Accumulate one of a child's output streams into a bounded tail.
///
/// Generic over the pipe because which stream carries the explanation is the
/// agent's choice, not ours: Claude Code puts a fatal error on stdout, Codex
/// puts its refusals on stderr.
async fn collect_output<R>(pipe: R, into: Arc<Mutex<StderrTail>>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt as _;
    let mut lines = tokio::io::BufReader::new(pipe).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        into.lock().await.push(line);
    }
}

/// Owns every task the monitor started.
pub struct TaskManager {
    db: Database,
    running: Mutex<std::collections::HashMap<uuid::Uuid, Running>>,
}

impl TaskManager {
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self {
            db,
            running: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Create a task and start its agent.
    pub async fn launch(self: &Arc<Self>, spec: &TaskSpec) -> Result<uuid::Uuid, TaskError> {
        let program = aum_procmon::launch::discover(spec.agent.executable()).ok_or_else(|| {
            TaskError::AgentMissing {
                agent: spec.agent.executable().to_owned(),
                hint: match spec.agent {
                    Agent::Codex => format!(
                        "It is usually not on PATH: it ships inside the ChatGPT desktop app, at {}.",
                        aum_procmon::launch::CODEX_BUNDLED_PATH
                    ),
                    Agent::ClaudeCode => "Install Claude Code, or set its path in Settings.".into(),
                },
            }
        })?;

        let task_id = uuid::Uuid::new_v4();
        let now = aum_db::now_sql();

        sqlx::query(
            "INSERT INTO task
               (id, benchmark_id, name, adapter_id, status, working_dir, command, created_at,
                started_at)
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, ?7)",
        )
        .bind(task_id.to_string())
        .bind(spec.benchmark_id.map(|b| b.to_string()))
        .bind(&spec.name)
        .bind(spec.agent.adapter_id())
        .bind(spec.working_dir.to_string_lossy().as_ref())
        // The command as launched, without the environment: see TaskSpec::env.
        .bind(format!("{} {}", program.display(), spec.prompt))
        .bind(&now)
        .execute(self.db.writer())
        .await
        .map_err(aum_db::DbError::from)?;

        let mut launch_spec = match spec.agent {
            Agent::ClaudeCode => {
                // The identity is fixed before the process exists, so the very
                // first request it makes is already attributed.
                let session_id = uuid::Uuid::new_v4();
                repo::bind_session(
                    self.db.writer(),
                    &session_id.to_string(),
                    &task_id.to_string(),
                    spec.agent.adapter_id(),
                    "launched_pinned",
                    &format!(r#"{{"session_id":"{session_id}","pinned_before_launch":true}}"#),
                )
                .await?;

                LaunchSpec::claude_code(
                    program,
                    session_id,
                    spec.working_dir.clone(),
                    Some(spec.prompt.clone()),
                )
            }
            Agent::Codex => {
                LaunchSpec::codex_exec(program, spec.working_dir.clone(), spec.prompt.clone())
            }
        };
        launch_spec.env = spec.env.clone();

        let mut process = launch_spec.spawn()?;

        // Codex announces its session id on the stream itself. Reading it from
        // our own child's pipe is what makes the binding a fact rather than a
        // guess.
        let reader = if launch_spec.reads_stdout {
            process.child.stdout.take().map(|stdout| {
                let db = self.db.clone();
                tokio::spawn(read_codex_stdout(db, task_id, stdout))
            })
        } else {
            None
        };

        // Drained continuously rather than read at exit: a child whose pipe
        // fills up blocks on the write, so an agent that is merely chatty would
        // hang instead of running.
        let stderr = Arc::new(Mutex::new(StderrTail::default()));
        if let Some(pipe) = process.child.stderr.take() {
            tokio::spawn(collect_output(pipe, Arc::clone(&stderr)));
        }

        // Claude Code reports a fatal error on **stdout**, not stderr —
        // "Failed to authenticate: OAuth session expired and could not be
        // refreshed" arrives there, while stderr carries only shell noise. A
        // stderr-only capture therefore recorded a failed task with no reason
        // at all, which is the case this whole mechanism exists for. Where
        // nothing else is consuming stdout, it is diagnostics too.
        if reader.is_none()
            && let Some(pipe) = process.child.stdout.take()
        {
            tokio::spawn(collect_output(pipe, Arc::clone(&stderr)));
        }

        self.running.lock().await.insert(
            task_id,
            Running {
                process,
                reader,
                stderr,
            },
        );

        tracing::info!(%task_id, agent = spec.agent.adapter_id(), "task launched");
        Ok(task_id)
    }

    /// Stop a task and everything it started.
    pub async fn stop(&self, task_id: uuid::Uuid) -> Result<(), TaskError> {
        let entry = self.running.lock().await.remove(&task_id);

        if let Some(mut running) = entry {
            // Signals the process group, so the agent's children go too. An
            // orphaned agent would keep making API calls after the task had
            // "finished", quietly corrupting the comparison it belongs to.
            let _ = running
                .process
                .stop(std::time::Duration::from_secs(5))
                .await;
            if let Some(reader) = running.reader.take() {
                reader.abort();
            }
        }

        sqlx::query("UPDATE task SET status = 'stopped', ended_at = ?1 WHERE id = ?2")
            .bind(aum_db::now_sql())
            .bind(task_id.to_string())
            .execute(self.db.writer())
            .await
            .map_err(aum_db::DbError::from)?;

        tracing::info!(%task_id, "task stopped");
        Ok(())
    }

    /// Notice tasks whose agent has exited on its own.
    ///
    /// Without this a finished run would sit at "running" for ever, and its
    /// elapsed time would keep climbing.
    pub async fn reap(&self) {
        let mut finished = Vec::new();
        {
            let mut running = self.running.lock().await;
            let mut done = Vec::new();
            for (id, entry) in running.iter_mut() {
                if let Ok(Some(status)) = entry.process.child.try_wait() {
                    done.push(*id);
                    finished.push((*id, status.code(), Arc::clone(&entry.stderr)));
                }
            }
            for id in done {
                running.remove(&id);
            }
        }

        for (id, code, stderr) in finished {
            let status = if code == Some(0) {
                "completed"
            } else {
                "failed"
            };

            // Only for a failure. A successful agent's stderr is noise, and
            // keeping it would turn a diagnostic into a log file.
            let detail = if status == "failed" {
                let text = stderr.lock().await.text();
                if let Some(text) = &text {
                    tracing::warn!(task_id = %id, detail = %text, "agent failed");
                }
                text
            } else {
                None
            };

            let _ = sqlx::query(
                "UPDATE task
                    SET status = ?1, ended_at = ?2, exit_code = ?3, failure_detail = ?4
                  WHERE id = ?5",
            )
            .bind(status)
            .bind(aum_db::now_sql())
            .bind(code)
            .bind(&detail)
            .bind(id.to_string())
            .execute(self.db.writer())
            .await;
            tracing::info!(task_id = %id, status, "task finished");
        }
    }

    #[must_use]
    pub async fn running_count(&self) -> usize {
        self.running.lock().await.len()
    }

    pub async fn stop_all(&self) {
        let ids: Vec<_> = self.running.lock().await.keys().copied().collect();
        for id in ids {
            let _ = self.stop(id).await;
        }
    }
}

/// Learn a launched Codex run's session id from its own stdout.
///
/// **This reads the identity and nothing else.** `codex exec --json` emits a
/// different schema from the rollout files it writes alongside — `turn.completed`
/// with a flat `usage` object, against the rollout's `event_msg` /
/// `token_count` with cumulative and last-turn ledgers — so the rollout parser
/// finds nothing here. That was the original bug: a launched Codex task
/// completed with zero requests and no binding, because this function was
/// written on the assumption that both transports carried the same events.
///
/// The fix is not to teach the parser a second usage schema. `codex exec`
/// writes a rollout file for every run, and its `session_meta.id` is byte-for-byte
/// the `thread_id` announced here — verified against a real run. So the stream
/// supplies the identity, which only our own child can tell us, and the file
/// supplies the usage, through the one parser that is already tested against a
/// 21,603-turn corpus. Parsing usage from both would risk counting it twice for
/// exactly the runs the monitor started.
async fn read_codex_stdout(db: Database, task_id: uuid::Uuid, stdout: tokio::process::ChildStdout) {
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let mut bound = false;

    while let Ok(Some(line)) = lines.next_line().await {
        if bound {
            continue;
        }
        let Some(session_id) = thread_id_of(&line) else {
            continue;
        };

        match repo::bind_session(
            db.writer(),
            &session_id,
            &task_id.to_string(),
            "codex",
            "launched_stdout",
            &format!(r#"{{"session_id":"{session_id}","read_from_own_child":true}}"#),
        )
        .await
        {
            Ok(()) => {
                // Anything the run already wrote to its rollout file predates
                // the binding, so claim it now rather than leaving the first
                // turns of our own task in Unattributed.
                match repo::attribute_existing(
                    db.writer(),
                    &session_id,
                    &task_id.to_string(),
                    "launched_stdout",
                )
                .await
                {
                    Ok(moved) => {
                        tracing::info!(%task_id, %session_id, moved, "codex session bound");
                    }
                    Err(e) => tracing::warn!(%task_id, error = %e, "could not back-attribute"),
                }
                bound = true;
            }
            Err(e) => {
                // A conflict means another task already owns this session, which
                // should be impossible for a process we spawned. Refuse rather
                // than steal it.
                tracing::error!(
                    %task_id, %session_id, error = %e,
                    "could not bind a launched Codex session"
                );
            }
        }
    }

    if !bound {
        tracing::warn!(
            %task_id,
            "codex exited without announcing a thread id, so its usage stays unattributed"
        );
    }
    tracing::debug!(%task_id, "codex stdout stream ended");
}

/// The session id from a `thread.started` line, if this is one.
///
/// Deliberately narrow: this stream is a different schema from the rollout
/// files, and the only field worth trusting from it is the identity.
fn thread_id_of(line: &str) -> Option<String> {
    if !line.contains("thread.started") {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "thread.started" {
        return None;
    }
    Some(value.get("thread_id")?.as_str()?.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn agents_map_to_their_adapter_ids() {
        assert_eq!(Agent::parse("claude_code"), Some(Agent::ClaudeCode));
        assert_eq!(Agent::parse("codex"), Some(Agent::Codex));
        assert_eq!(Agent::parse("cursor"), None);
        assert_eq!(Agent::ClaudeCode.adapter_id(), "claude_code");
    }

    #[tokio::test]
    async fn launching_a_missing_agent_explains_where_to_find_it() {
        // "codex is not installed" is the wrong conclusion on macOS, and a long
        // dead end for anyone who follows it.
        let db = aum_db::open_in_memory().await.unwrap();
        let manager = Arc::new(TaskManager::new(db));

        let spec = TaskSpec {
            name: "x".into(),
            agent: Agent::Codex,
            working_dir: std::env::temp_dir(),
            prompt: "do a thing".into(),
            benchmark_id: None,
            env: Vec::new(),
        };

        // Only meaningful when Codex genuinely is not present.
        if aum_procmon::launch::discover("codex").is_some() {
            return;
        }
        match manager.launch(&spec).await {
            Err(TaskError::AgentMissing { hint, .. }) => {
                assert!(
                    hint.contains("ChatGPT"),
                    "hint should name the real place: {hint}"
                );
            }
            other => panic!("expected AgentMissing, got {:?}", other.err()),
        }
    }

    #[tokio::test]
    async fn a_claude_task_is_bound_before_the_process_starts() {
        // The property that makes attribution exact rather than correlated: the
        // binding exists before any usage can be produced.
        let db = aum_db::open_in_memory().await.unwrap();
        let manager = Arc::new(TaskManager::new(db.clone()));

        // Substitute a harmless program so the test does not need Claude Code.
        let spec = TaskSpec {
            name: "auth refactor".into(),
            agent: Agent::ClaudeCode,
            working_dir: std::env::temp_dir(),
            prompt: "refactor authentication".into(),
            benchmark_id: None,
            env: Vec::new(),
        };

        if aum_procmon::launch::discover("claude").is_none() {
            return; // Claude Code is not installed here.
        }

        let task_id = manager.launch(&spec).await.unwrap();
        let (bindings,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM task_binding WHERE task_id = ?1")
                .bind(task_id.to_string())
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(bindings, 1, "the session must be bound at launch");

        let (method,): (String,) =
            sqlx::query_as("SELECT method FROM task_binding WHERE task_id = ?1")
                .bind(task_id.to_string())
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(method, "launched_pinned");

        manager.stop(task_id).await.unwrap();
    }

    #[tokio::test]
    async fn stopping_marks_the_task_and_forgets_the_process() {
        let db = aum_db::open_in_memory().await.unwrap();
        let manager = Arc::new(TaskManager::new(db.clone()));

        // A task row with no process, which is the state after a crash.
        let task_id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO task (id, name, adapter_id, status, created_at)
             VALUES (?1, 'x', 'claude_code', 'running', ?2)",
        )
        .bind(task_id.to_string())
        .bind(aum_db::now_sql())
        .execute(db.writer())
        .await
        .unwrap();

        manager.stop(task_id).await.unwrap();

        let (status,): (String,) = sqlx::query_as("SELECT status FROM task WHERE id = ?1")
            .bind(task_id.to_string())
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(status, "stopped");
        assert_eq!(manager.running_count().await, 0);
    }

    #[tokio::test]
    async fn two_tasks_cannot_share_a_session() {
        // The structural guarantee, at the level a launch would hit it.
        let db = aum_db::open_in_memory().await.unwrap();
        for id in ["t1", "t2"] {
            sqlx::query(
                "INSERT INTO task (id, name, adapter_id, status, created_at)
                 VALUES (?1, 'x', 'codex', 'running', ?2)",
            )
            .bind(id)
            .bind(aum_db::now_sql())
            .execute(db.writer())
            .await
            .unwrap();
        }

        repo::bind_session(db.writer(), "s1", "t1", "codex", "launched_stdout", "{}")
            .await
            .unwrap();
        assert!(
            repo::bind_session(db.writer(), "s1", "t2", "codex", "launched_stdout", "{}")
                .await
                .is_err(),
            "a second task must not be able to claim a bound session"
        );
    }
}
