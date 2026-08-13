//! Probing this machine's actual applications.
//!
//! Ignored by default: it depends on data that exists only where the agents
//! have really run. Read-only, and asserts on capability states rather than on
//! anything inside the files.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use aum_adapters::{claude_code::ClaudeCodeAdapter, codex::CodexAdapter};
use aum_contract::CapabilityState;

fn is_jsonl(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "jsonl")
}

fn is_rollout(p: &Path) -> bool {
    is_jsonl(p)
        && p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("rollout-"))
}

fn show(d: &aum_contract::AdapterDescriptor) {
    eprintln!("\n━━ {}  [{:?}]", d.display_name, d.state);
    for (name, cap) in &d.capabilities {
        let (mark, detail) = match cap {
            CapabilityState::Supported { evidence } => ("yes ", evidence.as_str()),
            CapabilityState::Degraded { caveat, .. } => ("~   ", caveat.as_str()),
            CapabilityState::Unsupported { reason } => ("no  ", reason.as_str()),
            CapabilityState::Unknown { reason } => ("?   ", reason.as_str()),
        };
        eprintln!("   {mark}{name:<28}{}", &detail[..detail.len().min(76)]);
    }
    for n in &d.notes {
        eprintln!("   note: {}", &n[..n.len().min(92)]);
    }
}

fn capability(d: &aum_contract::AdapterDescriptor, name: &str) -> CapabilityState {
    d.capabilities
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("no capability {name}"))
}

#[test]
#[ignore = "requires the agents' real data on this machine"]
fn the_matrix_reflects_what_this_machine_actually_reports() {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .expect("HOME");

    let started = std::time::Instant::now();

    let claude_root = home.join(".claude").join("projects");
    let claude = aum_engine::describe_file_adapter(
        "claude_code",
        "Claude Code",
        &aum_engine::probe(&ClaudeCodeAdapter, &claude_root, &is_jsonl),
        None,
        claude_root.is_dir(),
    );

    let codex_root = home.join(".codex").join("sessions");
    let codex = aum_engine::describe_file_adapter(
        "codex",
        "Codex",
        &aum_engine::probe(&CodexAdapter, &codex_root, &is_rollout),
        None,
        codex_root.is_dir(),
    );

    let desktop =
        aum_engine::describe_claude_desktop(Path::new("/Applications/Claude.app").is_dir());

    for d in [&claude, &codex, &desktop] {
        show(d);
    }
    eprintln!("\nprobed in {:?}\n", started.elapsed());

    // Probing runs on every visit to the Applications screen, so it has to be
    // cheap even against a gigabyte of history.
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "probing took {:?}, which is too slow to do on demand",
        started.elapsed()
    );

    // The asymmetry the whole product rests on, asserted against real data
    // rather than against a table someone wrote down.
    assert!(
        matches!(
            capability(&claude, "reasoning_tokens"),
            CapabilityState::Unsupported { .. }
        ),
        "Claude Code reports no reasoning tokens, and the matrix should conclude that from evidence"
    );
    assert!(
        matches!(
            capability(&codex, "reasoning_tokens"),
            CapabilityState::Supported { .. }
        ),
        "Codex does report reasoning tokens"
    );
    assert!(
        matches!(
            capability(&desktop, "exact_token_counts"),
            CapabilityState::Unsupported { .. }
        ),
        "Claude Desktop exposes no token telemetry at all"
    );
}
