//! Claude Code.
//!
//! Reads `~/.claude/projects/<slug>/<session-uuid>.jsonl`, which Claude Code
//! appends to in real time, one line per message. Assistant lines carry
//! `.message.usage` — Anthropic's own usage object, relayed verbatim.
//!
//! # The three things that make this non-obvious
//!
//! **One API response is written as many lines.** A response containing N
//! parallel tool calls becomes N JSONL lines, each repeating the *entire* usage
//! object, with different `uuid`s and timestamps spanning seconds. Measured on
//! a real session: 1,980 assistant lines carried only 1,135 distinct request
//! ids, and naively summing them inflates output tokens by **2.12x** and
//! cache-creation by **2.58x**. Deduplication by `(requestId, message.id)` is
//! not an optimisation; it is the difference between right and wrong.
//!
//! **Sub-agent work lives in other files.** `<session>/subagents/**.jsonl`
//! accounted for 76% of requests and 75% of cache-creation tokens in that same
//! session. Those lines carry the *parent* session id, so attributing on the
//! per-line `sessionId` picks them up for free — while attributing on file path
//! would miss them entirely.
//!
//! **Some lines are not requests at all.** `model: "<synthetic>"` marks a
//! terminal API failure, with zero usage and a non-`msg_` id; counting it as a
//! request corrupts failure rates. `compact_boundary` carries context-window
//! sizes around a million tokens, which are not billing figures.

mod parse;

use crate::{AdapterId, LineCtx, ParseOutcome, UsageAdapter};

pub use parse::parse_line;

pub const ADAPTER_ID: AdapterId = "claude_code";

/// Measurement source for a transcript-relayed usage object.
///
/// `ProviderReported`, not `ApplicationTelemetry`: the object contains
/// `service_tier` and `inference_geo`, which the CLI cannot compute and can only
/// have received. Classification follows authorship of the number, not the
/// medium it travelled through.
pub const SOURCE_TRANSCRIPT: &str = "provider_exact";

#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCodeAdapter;

impl UsageAdapter for ClaudeCodeAdapter {
    fn id(&self) -> AdapterId {
        ADAPTER_ID
    }

    /// Deliberately loose.
    ///
    /// The precise marker is `"type":"assistant"` (the JSON is written without
    /// spaces), but `"usage"` is also accepted so that a future formatting
    /// change costs a few wasted parses rather than silently dropping every
    /// measurement. `system` lines are needed for failures and compaction.
    fn is_candidate_line(&self, head: &[u8]) -> bool {
        memchr::memmem::find(head, b"\"usage\"").is_some()
            || memchr::memmem::find(head, b"\"type\":\"assistant\"").is_some()
            || memchr::memmem::find(head, b"\"type\":\"system\"").is_some()
            || memchr::memmem::find(head, b"\"isApiErrorMessage\"").is_some()
    }

    fn parse_line(&self, ctx: &mut LineCtx, line: &[u8]) -> ParseOutcome {
        parse::parse_line(ctx, line)
    }
}
