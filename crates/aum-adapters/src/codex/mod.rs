//! Codex.
//!
//! Reads `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. Every line is
//! `{timestamp, type, payload}`; the ones that matter are `session_meta`,
//! `turn_context` and `event_msg` with `payload.type == "token_count"`.
//!
//! Two things differ from Claude Code and both change the design:
//!
//! **Usage is cumulative.** Per-request rows come from the *advance* of a
//! running counter, not from the event itself. See [`reconcile`] — neither of
//! the two figures Codex reports is correct on its own, and they err in
//! opposite directions.
//!
//! **The model is not on the usage event.** It lives on `turn_context` and must
//! be carried forward. A tailer that starts mid-file genuinely does not know
//! which model produced a turn, and reports cost as unavailable rather than
//! attributing it to a guess.
//!
//! Codex does report reasoning tokens, which Claude Code does not — the one
//! place the comparison is asymmetric in Codex's favour.

pub mod reconcile;

mod parse;

use crate::{AdapterId, LineCtx, ParseOutcome, UsageAdapter};

pub use parse::parse_line;
pub use reconcile::{Reconciled, Watermark};

pub const ADAPTER_ID: AdapterId = "codex";

/// An ordinary turn, derived from the advance of the provider's own counter.
pub const SOURCE_CUMULATIVE_DELTA: &str = "provider_cumulative_delta";
/// A call the provider billed but excluded from its running total. Still the
/// provider's own numbers, so still exact — just not on its ledger.
pub const SOURCE_OFF_LEDGER: &str = "provider_off_ledger";

#[derive(Debug, Default, Clone, Copy)]
pub struct CodexAdapter;

impl UsageAdapter for CodexAdapter {
    fn id(&self) -> AdapterId {
        ADAPTER_ID
    }

    /// Only 0.09% of rollout bytes are relevant — the files are dominated by
    /// tool output, one line of which reaches 69.94 MiB. This test runs before
    /// any JSON parsing and is what makes reading a gigabyte of history cheap.
    fn is_candidate_line(&self, head: &[u8]) -> bool {
        memchr::memmem::find(head, b"\"token_count\"").is_some()
            || memchr::memmem::find(head, b"\"turn_context\"").is_some()
            || memchr::memmem::find(head, b"\"session_meta\"").is_some()
            || memchr::memmem::find(head, b"\"context_compacted\"").is_some()
    }

    fn parse_line(&self, ctx: &mut LineCtx, line: &[u8]) -> ParseOutcome {
        parse::parse_line(ctx, line)
    }
}
