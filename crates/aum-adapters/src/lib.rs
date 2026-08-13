//! # `aum-adapters` — reading each agent's own record of what it spent
//!
//! One module per monitored application. Each is isolated, so a change to
//! Claude Code's or Codex's on-disk format breaks one parser and one set of
//! golden tests rather than the application.
//!
//! ## The hot path is synchronous
//!
//! [`UsageAdapter::parse_line`] is a pure function: no I/O, no async, no
//! database. That is a deliberate structural choice rather than a style
//! preference — it means backfill can run on a blocking pool with no async
//! overhead, and every parser is testable against golden fixtures with zero
//! runtime and zero setup. Only lifecycle operations need to be async, and
//! those happen a handful of times per session.

pub mod claude_code;

use aum_domain::TokenUsage;

pub type AdapterId = &'static str;

/// What a parsed line tells us.
///
/// Deliberately more than just usage: a request that *failed* is as important
/// to record as one that succeeded, because a task containing an unmeasured
/// request must never present its total as exact.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    /// A session was observed, with whatever metadata came with it.
    SessionOpened {
        session_id: String,
        cwd: Option<String>,
        app_version: Option<String>,
    },

    /// The model in use changed, or was declared for the first time.
    ///
    /// Emitted separately from usage because Codex reports usage without a
    /// model: a tailer that starts mid-file legitimately does not know it, and
    /// cost must be unavailable rather than guessed.
    ModelDeclared {
        session_id: String,
        model_id: String,
    },

    /// The only carrier of token counts.
    Usage(Box<UsageSignal>),

    /// A request that terminally produced no usable response.
    ///
    /// Tokens are `None`, never zero: the provider reports no usage for a failed
    /// request, and we cannot assert that none were consumed.
    RequestFailed {
        session_id: String,
        dedup_key: String,
        occurred_at: chrono::DateTime<chrono::Utc>,
        detail: String,
    },

    /// One attempt in a retry chain. Grouped into incidents downstream — a run
    /// of 8 retries is one failure, not eight.
    RetryAttempt {
        session_id: String,
        attempt: u32,
        max_attempts: Option<u32>,
        occurred_at: chrono::DateTime<chrono::Utc>,
    },

    /// The conversation was compacted.
    ///
    /// Carries context-window sizes, **not** billing. The `preTokens` figure on
    /// a real boundary is ~999,000; adding it to a token total inflates that
    /// session by a million tokens for something that was never a request.
    ContextCompacted {
        session_id: String,
        pre_tokens: Option<u64>,
        post_tokens: Option<u64>,
    },

    /// Something did not add up. Recorded and surfaced, never silently fixed.
    Anomaly { kind: String, detail: String },
}

/// A token measurement, already normalized and already identified.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSignal {
    pub session_id: String,
    /// The identity that makes re-ingest idempotent. Provider-specific because
    /// the providers are: Claude Code has a request id, Codex has none.
    pub dedup_key: String,
    pub model_id: Option<String>,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub usage: TokenUsage,
    pub measurement_source: &'static str,
    pub request_kind: &'static str,
    /// Sub-agent work. Claude Code's sidechain lines carry the *parent*
    /// session id, so this attributes to the owning task automatically.
    pub is_sidechain: bool,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    /// The provider's own object, verbatim, as the audit trail.
    pub raw_json: Option<String>,
}

/// The result of looking at one line.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ParseOutcome {
    /// Not a line this adapter cares about.
    #[default]
    Ignored,
    Signals(Vec<Signal>),
    /// Structurally broken. Recorded as an anomaly; never silently dropped,
    /// because a parser that quietly discards what it cannot read will
    /// under-report without any symptom.
    Malformed {
        reason: String,
    },
}

impl ParseOutcome {
    #[must_use]
    pub fn one(signal: Signal) -> Self {
        Self::Signals(vec![signal])
    }
}

/// Per-file state carried across lines.
///
/// Codex needs this for its cumulative counters; Claude Code needs the current
/// model, which is declared on one line and applies to later ones.
#[derive(Debug, Clone, Default)]
pub struct LineCtx {
    pub current_model: Option<String>,
    pub current_session: Option<String>,
    /// Index of the current usage-bearing event within the file. Stable across
    /// re-reads, which is what makes it usable as an identity where the
    /// provider supplies none.
    pub event_ordinal: u64,
    /// Codex's previously seen cumulative counters.
    pub previous_cumulative: Option<aum_domain::OpenAiUsage>,
    pub previous_last: Option<aum_domain::OpenAiUsage>,
}

pub trait UsageAdapter: Send + Sync + 'static {
    fn id(&self) -> AdapterId;

    /// Cheap byte test run before any JSON parsing.
    ///
    /// Must be **conservative**: a false positive costs one wasted parse, a
    /// false negative silently loses a measurement.
    fn is_candidate_line(&self, head: &[u8]) -> bool;

    /// The hot path. Pure and synchronous — no I/O.
    fn parse_line(&self, ctx: &mut LineCtx, line: &[u8]) -> ParseOutcome;
}
