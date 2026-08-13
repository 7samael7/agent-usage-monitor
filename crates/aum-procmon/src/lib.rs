//! # `aum-procmon` — processes we launched, and processes we watch
//!
//! Two jobs, both of which look trivial and are not.
//!
//! **Identity.** A PID is reused. Binding a task to a bare PID means that, some
//! time after the agent exits, an unrelated process inherits its number and
//! starts contributing to a benchmark. Every process reference here is
//! `(pid, start_time)`, which is unique for the life of the machine. Claude
//! Code's own session files record `procStart` for the same reason.
//!
//! **Termination.** Agents spawn children — shells, language servers, sub-agent
//! processes — and killing only the process we started leaves those running.
//! In a monitoring tool that is not merely untidy: an orphaned agent keeps
//! making API calls, so a benchmark that has "finished" carries on spending, and
//! its comparison becomes meaningless. Launched processes therefore get their
//! own process group, and stopping a task signals the group.

pub mod launch;

use std::time::{Duration, SystemTime};

pub use launch::{LaunchError, LaunchSpec, LaunchedProcess};

/// A process identity that survives PID reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcKey {
    pub pid: u32,
    /// Seconds since the epoch, as the OS reports the process start.
    pub start_time: u64,
}

impl ProcKey {
    #[must_use]
    pub const fn new(pid: u32, start_time: u64) -> Self {
        Self { pid, start_time }
    }
}

/// A snapshot of one process.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessInfo {
    pub key: ProcKey,
    pub parent: Option<u32>,
    pub name: String,
    pub executable: Option<String>,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub started_at: SystemTime,
}

/// Aggregate resource use of a process and everything under it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TreeUsage {
    pub process_count: u32,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

/// Reads process state.
///
/// Wraps `sysinfo` so the refresh policy lives in one place: refreshing
/// everything at 1 Hz is a measurable cost in an application whose entire
/// purpose is to not perturb what it measures.
pub struct ProcessMonitor {
    system: sysinfo::System,
    last_refresh: Option<std::time::Instant>,
    min_interval: Duration,
}

impl Default for ProcessMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessMonitor {
    #[must_use]
    pub fn new() -> Self {
        Self {
            system: sysinfo::System::new(),
            last_refresh: None,
            // CPU percentages are computed between refreshes, so refreshing more
            // often than this produces noise rather than resolution.
            min_interval: Duration::from_millis(500),
        }
    }

    /// Refresh, unless it happened very recently.
    pub fn refresh(&mut self) {
        if let Some(last) = self.last_refresh
            && last.elapsed() < self.min_interval
        {
            return;
        }
        self.system
            .refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        self.last_refresh = Some(std::time::Instant::now());
    }

    /// Look up a process, confirming it is the *same* process we mean.
    ///
    /// Returns `None` when the PID exists but belongs to something else, which
    /// is exactly the case a bare-PID lookup would get wrong.
    #[must_use]
    pub fn get(&self, key: ProcKey) -> Option<ProcessInfo> {
        let pid = sysinfo::Pid::from_u32(key.pid);
        let process = self.system.process(pid)?;
        if process.start_time() != key.start_time {
            return None;
        }
        Some(Self::describe(pid, process))
    }

    /// Is this exact process still running?
    #[must_use]
    pub fn is_alive(&self, key: ProcKey) -> bool {
        self.get(key).is_some()
    }

    /// Find a currently-running process by PID, returning its full identity.
    ///
    /// Used when attaching to something the user points at: the caller supplies
    /// a PID, and this resolves the `(pid, start_time)` pair that will remain
    /// valid afterwards.
    #[must_use]
    pub fn identify(&self, pid: u32) -> Option<ProcessInfo> {
        let sys_pid = sysinfo::Pid::from_u32(pid);
        let process = self.system.process(sys_pid)?;
        Some(Self::describe(sys_pid, process))
    }

    /// Every descendant of a process, including itself.
    ///
    /// Walks children rather than parents, so a deep tree costs one pass.
    #[must_use]
    pub fn descendants(&self, root: ProcKey) -> Vec<ProcessInfo> {
        if !self.is_alive(root) {
            return Vec::new();
        }

        let mut wanted: std::collections::HashSet<u32> = std::collections::HashSet::new();
        wanted.insert(root.pid);

        // Repeat until no new members: a child may appear before its parent in
        // iteration order, and process tables are small enough that a few passes
        // cost nothing.
        loop {
            let mut grew = false;
            for (pid, process) in self.system.processes() {
                let Some(parent) = process.parent() else {
                    continue;
                };
                if wanted.contains(&parent.as_u32()) && wanted.insert(pid.as_u32()) {
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }

        wanted
            .into_iter()
            .filter_map(|pid| {
                let sys_pid = sysinfo::Pid::from_u32(pid);
                self.system
                    .process(sys_pid)
                    .map(|p| Self::describe(sys_pid, p))
            })
            .collect()
    }

    /// Resource use of a whole tree.
    #[must_use]
    pub fn tree_usage(&self, root: ProcKey) -> TreeUsage {
        let members = self.descendants(root);
        TreeUsage {
            process_count: u32::try_from(members.len()).unwrap_or(u32::MAX),
            cpu_percent: members.iter().map(|p| p.cpu_percent).sum(),
            memory_bytes: members
                .iter()
                .fold(0_u64, |acc, p| acc.saturating_add(p.memory_bytes)),
        }
    }

    /// This process, so the monitor's own cost is visible rather than assumed.
    #[must_use]
    pub fn own_usage(&self) -> Option<ProcessInfo> {
        self.identify(std::process::id())
    }

    fn describe(pid: sysinfo::Pid, process: &sysinfo::Process) -> ProcessInfo {
        ProcessInfo {
            key: ProcKey::new(pid.as_u32(), process.start_time()),
            parent: process.parent().map(sysinfo::Pid::as_u32),
            name: process.name().to_string_lossy().into_owned(),
            executable: process.exe().map(|p| p.to_string_lossy().into_owned()),
            cpu_percent: process.cpu_usage(),
            memory_bytes: process.memory(),
            started_at: SystemTime::UNIX_EPOCH + Duration::from_secs(process.start_time()),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn monitor() -> ProcessMonitor {
        let mut m = ProcessMonitor::new();
        m.refresh();
        m
    }

    #[test]
    fn finds_this_process() {
        let m = monitor();
        let me = m.identify(std::process::id()).expect("we are running");
        assert_eq!(me.key.pid, std::process::id());
        assert!(me.key.start_time > 0);
    }

    #[test]
    fn a_pid_with_the_wrong_start_time_is_not_the_same_process() {
        // The PID-reuse guard. Without it, a task bound to a finished agent
        // would silently start following whatever inherited its number.
        let m = monitor();
        let me = m.identify(std::process::id()).unwrap();

        let impostor = ProcKey::new(me.key.pid, me.key.start_time.saturating_add(1));
        assert!(m.get(me.key).is_some(), "the real process resolves");
        assert!(
            m.get(impostor).is_none(),
            "a different start time must not match"
        );
        assert!(!m.is_alive(impostor));
    }

    #[test]
    fn an_unknown_pid_resolves_to_nothing() {
        let m = monitor();
        // PIDs are 32-bit but capped far below this on every supported OS.
        assert!(m.identify(u32::MAX - 1).is_none());
    }

    #[test]
    fn a_process_is_its_own_descendant() {
        let m = monitor();
        let me = m.identify(std::process::id()).unwrap();
        let tree = m.descendants(me.key);
        assert!(
            tree.iter().any(|p| p.key == me.key),
            "the root must be included in its own tree"
        );
    }

    #[test]
    fn the_tree_of_a_dead_process_is_empty_rather_than_the_whole_machine() {
        // A bug here would attribute every process on the system to a task.
        let m = monitor();
        let dead = ProcKey::new(u32::MAX - 1, 0);
        assert!(m.descendants(dead).is_empty());
        assert_eq!(m.tree_usage(dead), TreeUsage::default());
    }

    #[test]
    fn tree_usage_counts_at_least_the_root() {
        let m = monitor();
        let me = m.identify(std::process::id()).unwrap();
        let usage = m.tree_usage(me.key);
        assert!(usage.process_count >= 1);
        assert!(usage.memory_bytes > 0);
    }

    #[test]
    fn the_monitor_can_measure_itself() {
        let m = monitor();
        assert!(
            m.own_usage().is_some(),
            "monitor overhead must be observable"
        );
    }
}
