//! Request/response bodies.

use serde::{Deserialize, Serialize};

use crate::measurement::{Measured, UnavailableReason};
use crate::money::{Currency, Money};
use crate::tokens::TokenBands;

// ── Meta ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HealthResponse {
    pub status: String,
    pub uptime_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct MetaResponse {
    pub contract_version: String,
    pub impl_name: String,
    pub impl_version: String,
    pub stream_epoch: uuid::Uuid,
    /// Backend features that are actually wired up. The UI hides what is absent
    /// rather than offering a control that does nothing.
    pub capabilities: Vec<String>,
}

// ── Tasks ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Stopped,
}

/// How a task is bound to a stream of usage events.
///
/// There is deliberately no `Heuristic` variant. If no binding can be
/// established, usage goes to the Unattributed bucket — it is never guessed
/// into a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TaskBinding {
    /// We spawned the agent and pinned its session identity.
    LaunchedPinned { session_id: String },
    /// We spawned the agent and read its event stream from our own child's stdout.
    LaunchedStdout { pid: u32 },
    /// The user bound an already-running process, proven via the agent's own
    /// PID→session file and validated against the process start time.
    AttachedPidSessionFile { pid: u32, session_id: String },
    /// The user explicitly bound a session UUID.
    SessionIdExact { session_id: String },
    /// Not yet bound. Usage will not be attributed here until it is.
    Unbound,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskSummary {
    pub id: uuid::Uuid,
    pub benchmark_id: Option<uuid::Uuid>,
    pub name: String,
    /// Adapter that owns this task, e.g. `claude_code` or `codex`.
    pub adapter_id: String,
    pub status: TaskStatus,
    pub binding: TaskBinding,
    pub working_dir: Option<String>,
    /// The model observed for this task. `None` until the first turn reports one
    /// — a tailer that starts mid-file legitimately does not know it yet.
    pub model_id: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// The authoritative snapshot the dashboard renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TaskMetrics {
    pub task_id: uuid::Uuid,
    pub status: TaskStatus,
    /// Cumulative bands for the task. Absolute, never a delta.
    pub bands: TokenBands,
    /// Total tokens with its provenance attached, so the UI can badge it.
    pub total_tokens: Measured<u64>,
    pub reasoning_tokens: Measured<u64>,
    pub requests: RequestCounts,
    pub elapsed_ms: u64,
    pub model_id: Option<String>,
    pub cost: CostBreakdown,
    /// Per-request latency, `Unavailable` unless a capture level that can
    /// actually measure it is enabled. Wall-clock between transcript writes is
    /// not latency: it contains tool execution, retry backoff and think time.
    pub latency: LatencySummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RequestCounts {
    pub succeeded: u32,
    /// Logical requests that terminally produced no usable response.
    pub failed: u32,
    /// Retried attempts, grouped into incidents. `None` where the agent does not
    /// expose retries at all — Codex does not, and rendering `0` there would
    /// make it look flawless when it is merely opaque.
    pub retries: Option<u32>,
    /// True when at least one usage event was coalesced, so counts are a floor
    /// and the UI must render `≥`.
    pub is_lower_bound: bool,
}

// ── Cost ─────────────────────────────────────────────────────────────────────

/// Three genuinely different quantities, never collapsed into one column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CostBreakdown {
    pub currency: Currency,
    /// What this usage *would* cost on pay-as-you-go, from our price table.
    pub api_equivalent: Measured<Money>,
    /// The agent's own figure, where it publishes one. A cross-check on ours.
    pub provider_reported: Measured<Money>,
    /// What the user was actually charged. Genuinely unknown under a
    /// subscription, and saying so is the whole point.
    pub actual_billed: Measured<Money>,
}

impl CostBreakdown {
    /// A breakdown for subscription-billed usage: an API-equivalent figure is
    /// computable, an actual charge is not.
    #[must_use]
    pub fn subscription(currency: Currency, api_equivalent: Measured<Money>, plan: &str) -> Self {
        Self {
            currency,
            api_equivalent,
            provider_reported: Measured::unavailable(UnavailableReason::NotReportedByProvider {
                field: "cost".to_owned(),
                detail: "This agent does not report a cost figure.".to_owned(),
            }),
            actual_billed: Measured::unavailable(UnavailableReason::SubscriptionBilled {
                plan: plan.to_owned(),
            }),
        }
    }
}

// ── Latency ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LatencySummary {
    pub average_ms: Measured<u64>,
    pub median_ms: Measured<u64>,
    pub p95_ms: Measured<u64>,
    pub time_to_first_token_ms: Measured<u64>,
    /// Output tokens per second. Only honest when latency is actually measured;
    /// dividing output by the gap between transcript writes is not this number.
    pub output_tokens_per_sec: Measured<u64>,
}

impl LatencySummary {
    /// Everything unavailable, with the reason and the remedy. This is the
    /// correct state for both file-tailing adapters.
    #[must_use]
    pub fn unavailable(detail: &str, level: &str) -> Self {
        let reason = || UnavailableReason::RequiresCaptureLevel {
            level: level.to_owned(),
            detail: detail.to_owned(),
        };
        Self {
            average_ms: Measured::unavailable(reason()),
            median_ms: Measured::unavailable(reason()),
            p95_ms: Measured::unavailable(reason()),
            time_to_first_token_ms: Measured::unavailable(reason()),
            output_tokens_per_sec: Measured::unavailable(reason()),
        }
    }
}

// ── Adapters ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdapterState {
    /// The application is installed and we can read its usage.
    Ready,
    /// Installed, but usage cannot be read. `detail` says why.
    Detected,
    NotInstalled,
    Error,
}

/// A capability, and the evidence for the claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CapabilityState {
    /// Observed in real data. `evidence` quotes what was seen.
    Supported { evidence: String },
    /// Works, with a caveat the user needs to know.
    Degraded { evidence: String, caveat: String },
    /// Confirmed absent, with the reason.
    Unsupported { reason: String },
    /// Not yet probed. Renders as `?`, never as yes.
    Unknown { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AdapterDescriptor {
    pub id: String,
    pub display_name: String,
    pub state: AdapterState,
    /// Version of the monitored application, where discoverable.
    pub app_version: Option<String>,
    pub adapter_version: String,
    /// Path we resolved for the executable, shown in diagnostics because it is
    /// routinely surprising — the Codex binary lives inside ChatGPT.app.
    pub executable_path: Option<String>,
    /// Capability name → state. Derived from observed evidence, not hardcoded.
    pub capabilities: Vec<(String, CapabilityState)>,
    /// Plain-language explanation shown on the diagnostics screen when something
    /// does not work.
    pub notes: Vec<String>,
}

// ── Ingest ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IngestProgress {
    pub files_done: u32,
    pub files_total: u32,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// How far behind live the tailer is.
    pub lag_ms: u64,
    pub anomalies: u32,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn subscription_cost_never_claims_an_actual_charge() {
        let c = CostBreakdown::subscription(
            Currency::Usd,
            Measured::calculated(
                Money::new(rust_decimal::Decimal::new(48, 2)),
                crate::measurement::MeasurementSource::ApplicationTelemetry,
            ),
            "team",
        );
        assert!(c.actual_billed.value.is_none());
        assert!(c.api_equivalent.value.is_some());
    }

    #[test]
    fn unmeasurable_latency_is_unavailable_with_a_remedy() {
        let l = LatencySummary::unavailable(
            "Claude Code transcripts contain no latency field.",
            "the local proxy",
        );
        assert!(l.p95_ms.value.is_none());
        let s = match &l.p95_ms.accuracy {
            crate::measurement::Accuracy::Unavailable { reason } => reason.sentence(),
            other => panic!("expected Unavailable, got {other:?}"),
        };
        assert!(s.contains("proxy"), "reason should name the remedy: {s}");
    }
}

// ── Observed usage ──────────────────────────────────────────────────────────

/// One agent session the monitor has read, whether or not a task claims it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SessionSummary {
    pub session_id: String,
    pub adapter_id: String,
    pub model_id: Option<String>,
    pub requests: u32,
    pub bands: TokenBands,
    pub total_tokens: Measured<u64>,
    pub reasoning_tokens: Measured<u64>,
    pub first_at: Option<String>,
    pub last_at: Option<String>,
    /// True when no task has claimed this session. Surfaced rather than hidden:
    /// what the application declined to guess about is information.
    pub unattributed: bool,
}

/// What ingest has read so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IngestStatus {
    pub passes: u64,
    pub files_scanned: u32,
    pub requests_recorded: u64,
    pub anomalies: u32,
    /// True until the first pass over existing history completes, so the UI can
    /// distinguish "nothing here" from "still reading".
    pub backfilling: bool,
}
