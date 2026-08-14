//! Turning stored rows into the shapes an interface renders.
//!
//! This is where a database row acquires its accuracy. `SessionTotals` knows how
//! many requests reported a reasoning count and how many did not; only here does
//! that become `Exact`, `Partial` or `Unavailable` — and once it has, no display
//! code can turn a missing count back into a zero.
//!
//! It lived inside the HTTP handlers until the HTTP layer was deleted. None of
//! it was ever about HTTP.

use std::path::{Path, PathBuf};

use aum_contract::{
    AdapterDescriptor, Measured, MeasurementSource, SessionSummary, TokenBands, UnavailableReason,
};

/// One recorded session, with its certainty attached.
#[must_use]
pub fn session_summary(t: aum_db::repo::SessionTotals) -> SessionSummary {
    let n = |v: i64| u64::try_from(v).unwrap_or(0);
    let bands = TokenBands {
        input_fresh: n(t.input_fresh),
        cache_read: n(t.cache_read),
        cache_write_5m: n(t.cache_write_5m),
        cache_write_1h: n(t.cache_write_1h),
        cache_write_unspecified: n(t.cache_write_unspecified),
        output_total: n(t.output_total),
        reasoning: t.reasoning.map(n),
        unclassified: n(t.unclassified),
    };

    let requests = u32::try_from(t.requests).unwrap_or(u32::MAX);
    let reported_by = u32::try_from(t.reasoning_reported_by).unwrap_or(u32::MAX);

    // These sessions were read from files the agents wrote, so the counts are
    // the provider's own.
    let total_tokens = Measured::exact(bands.grand_total(), MeasurementSource::ProviderReported);

    let reasoning_tokens = match t.reasoning {
        None => Measured::unavailable(UnavailableReason::NotReportedByProvider {
            field: "reasoning_tokens".to_owned(),
            detail: "This agent does not report a reasoning-token count.".to_owned(),
        }),
        Some(v) if reported_by >= requests => {
            Measured::exact(n(v), MeasurementSource::ProviderReported)
        }
        // Some requests reported it and some did not, so the total is a floor.
        // Rendering it as a plain number would overstate what is known.
        Some(v) => Measured::partial(
            n(v),
            reported_by,
            requests,
            format!("{reported_by} of {requests} requests report reasoning tokens"),
        ),
    };

    SessionSummary {
        session_id: t.session_id,
        adapter_id: t.adapter_id,
        model_id: t.model_id,
        requests,
        bands,
        total_tokens,
        reasoning_tokens,
        first_at: t.first_at,
        last_at: t.last_at,
        unattributed: t.unattributed,
    }
}

/// Where an agent's executable actually is.
///
/// `PATH` first, then the known bundled location — the Codex binary is not on
/// `PATH` at all on a normal install; it ships inside ChatGPT.app. Returns
/// `None` rather than a guess, so the matrix can say "not found" instead of
/// showing a path that does not exist.
#[must_use]
pub fn discover_executable(program: &str) -> Option<PathBuf> {
    if let Some(found) = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join(program))
            .find(|c| c.is_file())
    }) {
        return Some(found);
    }
    if program == "codex" {
        let bundled = PathBuf::from(CODEX_BUNDLED_PATH);
        if bundled.is_file() {
            return Some(bundled);
        }
    }
    None
}

/// Codex ships inside the ChatGPT desktop application rather than on `PATH`.
pub const CODEX_BUNDLED_PATH: &str = "/Applications/ChatGPT.app/Contents/Resources/codex";

fn is_jsonl(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "jsonl")
}

fn is_rollout(p: &Path) -> bool {
    is_jsonl(p)
        && p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("rollout-"))
}

/// The capability matrix, derived by reading each application's own files.
///
/// Blocking: it reads the tail of several transcripts. Callers on an async
/// runtime should wrap it in `spawn_blocking` rather than stalling the reactor.
#[must_use]
pub fn describe_adapters(
    home: &Path,
    desktop_daily: Option<aum_contract::DailyTotal>,
) -> Vec<AdapterDescriptor> {
    use aum_adapters::{claude_code::ClaudeCodeAdapter, codex::CodexAdapter};

    let claude_root = home.join(".claude").join("projects");
    let codex_root = home.join(".codex").join("sessions");

    vec![
        crate::probe::describe_file_adapter(
            "claude_code",
            "Claude Code",
            &crate::probe::probe(&ClaudeCodeAdapter, &claude_root, &is_jsonl),
            discover_executable("claude").map(|p| p.display().to_string()),
            claude_root.is_dir(),
        ),
        crate::probe::describe_file_adapter(
            "codex",
            "Codex",
            &crate::probe::probe(&CodexAdapter, &codex_root, &is_rollout),
            discover_executable("codex").map(|p| p.display().to_string()),
            codex_root.is_dir(),
        ),
        crate::probe::describe_claude_desktop(
            Path::new("/Applications/Claude.app").is_dir(),
            desktop_daily,
        ),
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use aum_contract::DisplayKind;

    fn totals(
        requests: i64,
        reasoning: Option<i64>,
        reported_by: i64,
    ) -> aum_db::repo::SessionTotals {
        aum_db::repo::SessionTotals {
            session_id: "s".to_owned(),
            adapter_id: "claude_code".to_owned(),
            model_id: Some("claude-opus-5".to_owned()),
            requests,
            input_fresh: 10,
            cache_read: 20,
            cache_write_5m: 0,
            cache_write_1h: 30,
            cache_write_unspecified: 0,
            output_total: 40,
            unclassified: 0,
            reasoning,
            reasoning_reported_by: reported_by,
            failed: 0,
            first_at: None,
            last_at: None,
            unattributed: true,
        }
    }

    #[test]
    fn an_agent_that_reports_no_reasoning_is_unavailable_rather_than_zero() {
        let s = session_summary(totals(5, None, 0));
        assert_eq!(s.reasoning_tokens.value, None);
        assert_eq!(s.reasoning_tokens.display_kind(), DisplayKind::Unavailable);
    }

    #[test]
    fn reasoning_reported_by_every_request_is_exact() {
        let s = session_summary(totals(5, Some(99), 5));
        assert_eq!(s.reasoning_tokens.value, Some(99));
        assert_eq!(s.reasoning_tokens.display_kind(), DisplayKind::Exact);
    }

    #[test]
    fn reasoning_reported_by_some_requests_is_a_floor_and_says_so() {
        // The number is real but incomplete. Rendering it plain would claim the
        // session used 99 reasoning tokens; it used at least 99.
        let s = session_summary(totals(5, Some(99), 2));
        assert_eq!(s.reasoning_tokens.value, Some(99));
        assert_eq!(s.reasoning_tokens.display_kind(), DisplayKind::Partial);
        let sentence = format!("{:?}", s.reasoning_tokens.accuracy);
        assert!(sentence.contains("2 of 5"), "{sentence}");
    }

    #[test]
    fn the_band_total_is_the_sum_of_disjoint_buckets() {
        let s = session_summary(totals(1, None, 0));
        assert_eq!(s.total_tokens.value, Some(10 + 20 + 30 + 40));
    }

    #[test]
    fn a_program_that_is_not_installed_is_none_rather_than_a_guess() {
        assert_eq!(
            discover_executable("definitely-not-a-real-binary-xyz"),
            None
        );
    }
}
