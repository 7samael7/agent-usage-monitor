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
    /// Files examined and found unchanged. High relative to `files_scanned` is
    /// the healthy state: it means idle passes are nearly free.
    pub files_skipped: u32,
    pub requests_recorded: u64,
    pub anomalies: u32,
    /// True until the first pass over existing history completes, so the UI can
    /// distinguish "nothing here" from "still reading".
    pub backfilling: bool,
}

/// One time bucket of usage, for a chart.
///
/// Bucketed by the backend: a task can have tens of thousands of requests, and
/// a chart needs a few hundred points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SeriesPoint {
    pub at: String,
    pub requests: u32,
    pub input_fresh: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output_total: u64,
    pub unclassified: u64,
}

// ── Pricing ─────────────────────────────────────────────────────────────────

/// One model this machine has actually used.
///
/// Not a catalogue of everything a provider publishes: the list that matters is
/// what ran here, because that is what needs a price.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ObservedModel {
    pub model_id: String,
    pub adapter_id: String,
    pub requests: u32,
    pub total_tokens: u64,
    /// Whether a rate exists for this exact id. Never true because a similar
    /// model has one.
    pub priced: bool,
}

/// One version of one model's rates, in money per million tokens.
///
/// Rates are amounts of money and so cross the wire as decimal strings, for the
/// same reason costs do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PriceRow {
    pub version_id: String,
    pub model_id: String,
    pub input_per_mtok: Money,
    pub output_per_mtok: Money,
    pub cache_read_per_mtok: Money,
    pub cache_write_5m_per_mtok: Money,
    pub cache_write_1h_per_mtok: Money,
    pub effective_from: String,
    /// `seed`, `user` or `updater`.
    pub source: String,
    /// True for the version a request made now would be costed with.
    pub is_current: bool,
}

/// An exchange rate, and how much to trust it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FxRow {
    pub quote_currency: String,
    /// Units of the quote currency per 1 USD.
    pub rate: Money,
    pub as_of: String,
    pub source: String,
    pub age_days: i64,
    /// Past a week old. Amounts converted with a stale rate are downgraded to
    /// estimates rather than shown as current.
    pub is_stale: bool,
    /// A full sentence for the interface, so the reason travels with the fact.
    pub description: String,
}

/// The pricing screen's whole state in one response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PricingView {
    pub models: Vec<ObservedModel>,
    pub prices: Vec<PriceRow>,
    pub fx: Vec<FxRow>,
    /// Currencies this backend can present. The interface offers exactly these
    /// rather than a hardcoded list that might not match.
    pub supported_currencies: Vec<String>,
}

/// A rate the user has entered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NewPrice {
    pub model_id: String,
    pub input_per_mtok: Money,
    pub output_per_mtok: Money,
    /// Optional because a provider may not charge separately for these. Absent
    /// means "same as input", which is both providers' documented default —
    /// not zero, which would understate a cached session by most of its total.
    pub cache_read_per_mtok: Option<Money>,
    pub cache_write_5m_per_mtok: Option<Money>,
    pub cache_write_1h_per_mtok: Option<Money>,
    pub note: Option<String>,
}

/// An exchange rate the user has entered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NewFxRate {
    pub quote_currency: String,
    pub rate: Money,
}

// ── Comparison ──────────────────────────────────────────────────────────────

/// One task's figures divided by the work it did.
///
/// Comparing raw totals answers "which task was bigger", which is rarely the
/// question. Dividing by output tokens answers "which agent was more expensive
/// for the same amount of produced text", which is.
///
/// There is deliberately **no normalized duration**. Wall-clock time between
/// transcript writes includes tool execution, retry backoff and think time, so
/// "milliseconds per 1,000 output tokens" would look like a throughput
/// measurement while being mostly a measurement of how long a file search took.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Normalized {
    /// A full sentence naming the basis, so a column header cannot drift from
    /// what was actually divided.
    pub basis: String,
    /// The divisor, shown so a reader can check the arithmetic.
    pub denominator: u64,
    pub total_tokens: Measured<u64>,
    pub input_tokens: Measured<u64>,
    pub cost: Measured<Money>,
}

/// One row of a comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ComparisonRow {
    pub task_id: uuid::Uuid,
    pub name: String,
    pub metrics: Box<TaskMetrics>,
    /// `None` when the task produced no output to divide by. Not zero, and not
    /// infinity: there is simply nothing to say yet.
    pub normalized: Option<Normalized>,
}

/// Tasks side by side, plus what makes them incomparable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Comparison {
    pub rows: Vec<ComparisonRow>,
    /// Reasons these rows are not straightforwardly comparable, in full
    /// sentences. Computed from the rows themselves rather than assumed, and
    /// shown next to the table rather than in a tooltip: a screenshot of a
    /// comparison must not be more confident than the comparison was.
    pub caveats: Vec<String>,
}
