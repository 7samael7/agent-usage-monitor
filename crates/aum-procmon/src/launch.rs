//! Launching an agent, and stopping it completely.
//!
//! Launched mode is what makes attribution exact. When the monitor starts the
//! agent itself it can pin the identity the usage will be reported under, so
//! there is nothing to correlate and nothing to guess:
//!
//! * **Claude Code** — we generate the session UUID and pass `--session-id`.
//!   Sub-agent transcripts carry the same parent session id, so they attribute
//!   automatically.
//! * **Codex** — `codex exec --json` writes its event stream to *our own
//!   child's stdout*. Same events, same parser, no filesystem correlation at
//!   all. (The binary is not on `PATH` on macOS; it ships inside ChatGPT.app.)
//!
//! Stopping is the other half. An agent spawns children, and signalling only
//! the process we started leaves them running — which in this application means
//! a "finished" benchmark task quietly carries on making API calls. Launched
//! processes therefore get their own process group and are signalled as a group.

use std::path::PathBuf;
use std::process::Stdio;

use crate::ProcKey;

/// Where to find each agent.
///
/// Discovery order matters more than it looks: the Codex binary is not on
/// `PATH` on macOS — it lives inside ChatGPT.app, which is also the Codex
/// desktop app — so a plain `which codex` finds nothing and the obvious
/// conclusion ("Codex is not installed") is wrong.
pub const CODEX_BUNDLED_PATH: &str = "/Applications/ChatGPT.app/Contents/Resources/codex";

/// How to start one agent.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    /// Extra environment for the child.
    ///
    /// Values are never persisted and never returned to the renderer: they
    /// routinely contain API keys, and a monitoring tool has no business
    /// keeping them.
    pub env: Vec<(String, String)>,
    /// True when the agent writes its event stream to stdout and we should read
    /// it, rather than tailing a file.
    pub reads_stdout: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("{program} was not found. {hint}")]
    NotFound { program: String, hint: String },
    #[error("the working directory {0} does not exist")]
    NoWorkingDir(String),
    #[error("could not start the process: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("the process started but reported no PID")]
    NoPid,
}

/// A running agent we own.
pub struct LaunchedProcess {
    pub key: ProcKey,
    pub child: tokio::process::Child,
    /// The process group, which is the PID of the leader — the child itself,
    /// because it was spawned into a new group.
    pub process_group: u32,
}

impl LaunchSpec {
    /// A Claude Code session whose identity we chose in advance.
    ///
    /// Passing `--session-id` is what makes this exact: usage arrives tagged
    /// with a UUID we already recorded against the task, so nothing has to be
    /// correlated by working directory or timing. That matters because working
    /// directory is not an identity — three Claude Code sessions were running in
    /// the same directory on this machine when the format was investigated.
    #[must_use]
    pub fn claude_code(
        program: PathBuf,
        session_id: uuid::Uuid,
        working_dir: PathBuf,
        prompt: Option<String>,
    ) -> Self {
        let mut args = vec!["--session-id".to_owned(), session_id.to_string()];
        if let Some(prompt) = prompt {
            args.push("--print".to_owned());
            args.push(prompt);
        }
        Self {
            program,
            args,
            working_dir,
            env: Vec::new(),
            // Usage is read from the transcript, which carries sub-agent work
            // too; stdout would only show the top-level session.
            reads_stdout: false,
        }
    }

    /// A Codex run whose events we read directly from its stdout.
    ///
    /// Deterministic by construction: it is our own child's pipe, so there is
    /// no window in which another Codex session could be mistaken for this one.
    #[must_use]
    pub fn codex_exec(program: PathBuf, working_dir: PathBuf, prompt: String) -> Self {
        Self {
            program,
            args: vec![
                "exec".to_owned(),
                "--json".to_owned(),
                "--cd".to_owned(),
                working_dir.to_string_lossy().into_owned(),
                prompt,
            ],
            working_dir,
            env: Vec::new(),
            reads_stdout: true,
        }
    }

    /// Start it.
    pub fn spawn(&self) -> Result<LaunchedProcess, LaunchError> {
        if !self.working_dir.is_dir() {
            return Err(LaunchError::NoWorkingDir(
                self.working_dir.display().to_string(),
            ));
        }

        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&self.working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Do not detach: if the monitor dies, the agent should not be left
            // running and billing.
            .kill_on_drop(true);

        for (key, value) in &self.env {
            command.env(key, value);
        }

        // A new process group, so the whole tree can be signalled later. Without
        // this, stopping a task leaves the agent's children alive and spending.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }

        let child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                LaunchError::NotFound {
                    program: self.program.display().to_string(),
                    hint: hint_for(&self.program),
                }
            } else {
                LaunchError::Spawn(e)
            }
        })?;

        let pid = child.id().ok_or(LaunchError::NoPid)?;

        // Read the start time back from the OS rather than recording "now": the
        // pair must match exactly what a later lookup will see, or the PID-reuse
        // guard rejects our own process.
        let mut monitor = crate::ProcessMonitor::new();
        monitor.refresh();
        let start_time = monitor.identify(pid).map_or(0, |p| p.key.start_time);

        Ok(LaunchedProcess {
            key: ProcKey::new(pid, start_time),
            child,
            process_group: pid,
        })
    }
}

fn hint_for(program: &std::path::Path) -> String {
    let name = program
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.as_str() {
        "codex" => format!(
            "The Codex binary is usually not on PATH: it ships inside the ChatGPT desktop app, at \
             {CODEX_BUNDLED_PATH}. Set the executable path for the Codex application in Settings."
        ),
        "claude" => "Install Claude Code, or set its executable path in Settings.".to_owned(),
        _ => "Check the executable path configured for this application.".to_owned(),
    }
}

/// Where an agent's executable actually is.
///
/// Checks `PATH` first, then the known bundled locations. Returns `None` rather
/// than a guess, so the UI can say the application was not found instead of
/// failing later with a confusing spawn error.
#[must_use]
pub fn discover(program: &str) -> Option<PathBuf> {
    if let Some(path) = which_on_path(program) {
        return Some(path);
    }
    if program == "codex" {
        let bundled = PathBuf::from(CODEX_BUNDLED_PATH);
        if bundled.is_file() {
            return Some(bundled);
        }
    }
    None
}

fn which_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

impl LaunchedProcess {
    /// Stop the agent and everything it started.
    ///
    /// `SIGTERM` to the group, a grace period, then `SIGKILL` to the group.
    /// Signalling the group rather than the child is the whole point: an agent's
    /// children outliving the task would keep making API calls, so a benchmark
    /// that reported "finished" would go on spending and its comparison would be
    /// wrong in a way nothing on screen would reveal.
    pub async fn stop(&mut self, grace: std::time::Duration) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            signal_group(self.process_group, nix::sys::signal::Signal::SIGTERM);

            if tokio::time::timeout(grace, self.child.wait()).await.is_ok() {
                // Exited politely, but children may still be up; the group
                // signal above already reached them.
                return Ok(());
            }
            signal_group(self.process_group, nix::sys::signal::Signal::SIGKILL);
        }

        #[cfg(not(unix))]
        {
            let _ = grace;
        }

        self.child.start_kill().or_else(|e| {
            // Already gone is success, not failure.
            if e.kind() == std::io::ErrorKind::InvalidInput {
                Ok(())
            } else {
                Err(e)
            }
        })?;
        let _ = self.child.wait().await;
        Ok(())
    }
}

#[cfg(unix)]
fn signal_group(pgid: u32, signal: nix::sys::signal::Signal) {
    let Ok(pgid) = i32::try_from(pgid) else {
        return;
    };
    // A failure here almost always means the group is already gone.
    if let Err(e) = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), signal) {
        tracing::debug!(%pgid, ?signal, error = %e, "could not signal the process group");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::time::Duration;

    fn shell_spec(script: &str) -> LaunchSpec {
        LaunchSpec {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_owned(), script.to_owned()],
            working_dir: std::env::temp_dir(),
            env: Vec::new(),
            reads_stdout: true,
        }
    }

    #[test]
    fn a_claude_launch_pins_the_session_identity() {
        // This is what makes attribution exact rather than correlated.
        let id = uuid::Uuid::new_v4();
        let spec = LaunchSpec::claude_code(
            PathBuf::from("/usr/local/bin/claude"),
            id,
            std::env::temp_dir(),
            None,
        );
        assert_eq!(spec.args.first().map(String::as_str), Some("--session-id"));
        assert_eq!(
            spec.args.get(1).map(String::as_str),
            Some(id.to_string().as_str())
        );
        // Usage comes from the transcript, which includes sub-agent work.
        assert!(!spec.reads_stdout);
    }

    #[test]
    fn a_codex_launch_reads_its_own_childs_stdout() {
        let spec = LaunchSpec::codex_exec(
            PathBuf::from(CODEX_BUNDLED_PATH),
            std::env::temp_dir(),
            "do the thing".to_owned(),
        );
        assert_eq!(spec.args.first().map(String::as_str), Some("exec"));
        assert!(spec.args.iter().any(|a| a == "--json"));
        assert!(
            spec.reads_stdout,
            "attribution depends on reading our own pipe"
        );
    }

    #[test]
    fn a_missing_program_explains_where_codex_actually_lives() {
        // "codex is not installed" is the wrong conclusion on macOS and would
        // send someone down a long dead end.
        let hint = hint_for(std::path::Path::new("/nowhere/codex"));
        assert!(
            hint.contains("ChatGPT"),
            "hint should name the real location: {hint}"
        );
        assert!(hint.contains(CODEX_BUNDLED_PATH));
    }

    #[test]
    fn discovery_returns_nothing_rather_than_a_guess() {
        assert!(discover("definitely-not-a-real-agent-binary").is_none());
    }

    #[test]
    fn discovery_finds_something_that_is_on_path() {
        assert!(discover("sh").is_some() || discover("ls").is_some());
    }

    #[tokio::test]
    async fn a_launched_process_gets_a_verifiable_identity() {
        let mut launched = shell_spec("sleep 5").spawn().unwrap();
        assert!(launched.key.pid > 0);
        assert!(
            launched.key.start_time > 0,
            "start time must be read back from the OS, or the reuse guard rejects our own child"
        );

        let mut monitor = crate::ProcessMonitor::new();
        monitor.refresh();
        assert!(monitor.is_alive(launched.key));

        launched.stop(Duration::from_millis(200)).await.unwrap();
    }

    #[tokio::test]
    async fn a_nonexistent_working_directory_fails_before_spawning() {
        let mut spec = shell_spec("true");
        spec.working_dir = PathBuf::from("/definitely/not/a/directory");
        assert!(matches!(spec.spawn(), Err(LaunchError::NoWorkingDir(_))));
    }

    #[tokio::test]
    async fn a_missing_binary_is_reported_with_a_hint() {
        let mut spec = shell_spec("true");
        spec.program = PathBuf::from("/nowhere/codex");
        match spec.spawn() {
            Err(LaunchError::NotFound { hint, .. }) => assert!(hint.contains("ChatGPT")),
            other => panic!("expected NotFound, got {other:?}", other = other.err()),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stopping_a_task_kills_the_children_too() {
        // The bug this prevents: an agent's children outlive the task and keep
        // making API calls, so a "finished" benchmark goes on spending.
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild.pid");

        // The shell spawns a long-lived grandchild and records its PID, then
        // waits. Killing only the shell would leave the grandchild running.
        let script = format!("sleep 120 & echo $! > {}; sleep 120", marker.display());
        let mut launched = shell_spec(&script).spawn().unwrap();

        // Wait for the grandchild's PID to be written.
        let mut grandchild = None;
        for _ in 0..50 {
            if let Ok(text) = std::fs::read_to_string(&marker)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                grandchild = Some(pid);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let grandchild = grandchild.expect("the grandchild should have recorded its PID");

        let mut monitor = crate::ProcessMonitor::new();
        monitor.refresh();
        assert!(
            monitor.identify(grandchild).is_some(),
            "the grandchild should be running before we stop the task"
        );

        launched.stop(Duration::from_millis(300)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;

        let mut after = crate::ProcessMonitor::new();
        after.refresh();
        assert!(
            after.identify(grandchild).is_none(),
            "the grandchild survived; an orphaned agent would keep spending tokens"
        );
    }
}
