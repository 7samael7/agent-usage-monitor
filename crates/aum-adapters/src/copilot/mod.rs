//! GitHub Copilot.
//!
//! Copilot is the first adapter here whose application writes **nothing
//! measurable by default**, and the design follows from that.
//!
//! What it writes without being asked:
//!
//! | Path | Contents | Tokens |
//! |---|---|---|
//! | `~/.copilot/jb/<session>/partition-*.jsonl` | JetBrains session events | none |
//! | `~/.copilot/logs/process-*.log` | process lifecycle | none |
//! | `~/.config/github-copilot/*.db` | auth, editor state, LSP caches | none |
//! | VS Code `github.copilot-chat/session-store.db` | sessions, turns | none |
//!
//! Measured on a real machine: 1,295 events across 14 JetBrains sessions, and
//! not one token count in any of them. VS Code's chat sessions carry
//! `maxInputTokens`, which is the model's context limit and not a measurement
//! of anything consumed — the kind of field that yields a confident wrong
//! number if read by name.
//!
//! What it writes **when asked**: the CLI exports OpenTelemetry, and the file
//! exporter puts OTLP/JSON in `~/.copilot/otel/*.jsonl` with real per-request
//! token counts. That needs three environment variables set before the session
//! starts, so history from before they were set does not exist and cannot be
//! recovered. See [`ENABLE_HINT`].
//!
//! ## Only `chat` spans are counted
//!
//! Copilot emits `invoke_agent` spans carrying **session totals** and `chat`
//! spans carrying **per-call** counts, using the same attribute names. Summing
//! both double-counts every session exactly — the same shape of error as
//! Claude Code's fan-out, and just as invisible in the result. Only `chat` is
//! counted. If a future version stops emitting them the total drops to zero,
//! which is wrong in the direction that gets noticed.

mod parse;

use crate::{AdapterId, LineCtx, ParseOutcome, UsageAdapter};

pub use parse::parse_line;

pub const ADAPTER_ID: AdapterId = "copilot";

/// Provider-authored counts, read from the agent's own telemetry export.
pub const SOURCE_OTEL_SPAN: &str = "provider_otel_span";

/// What to set so that Copilot records anything at all.
///
/// Carried in the code rather than only in documentation because the
/// Applications view shows it: "unsupported" is a dead end, and this is the
/// one sentence that turns it into something the reader can act on.
pub const ENABLE_HINT: &str = "Copilot writes no token counts unless its OpenTelemetry file \
                               exporter is on. Set COPILOT_OTEL_ENABLED=true, \
                               COPILOT_OTEL_EXPORTER_TYPE=file and \
                               COPILOT_OTEL_FILE_EXPORTER_PATH=~/.copilot/otel/copilot.jsonl \
                               before starting a session. Earlier sessions recorded nothing and \
                               cannot be recovered.";

#[derive(Debug, Default, Clone, Copy)]
pub struct CopilotAdapter;

impl UsageAdapter for CopilotAdapter {
    fn id(&self) -> AdapterId {
        ADAPTER_ID
    }

    /// Every OTLP/JSON line opens with its envelope key, well inside the
    /// prefilter window, so this cannot produce a false negative on a real
    /// export. The usage attribute is checked too, for exporters that write a
    /// bare span per line.
    fn is_candidate_line(&self, head: &[u8]) -> bool {
        memchr::memmem::find(head, b"resourceSpans").is_some()
            || memchr::memmem::find(head, b"gen_ai.usage.").is_some()
    }

    fn parse_line(&self, ctx: &mut LineCtx, line: &[u8]) -> ParseOutcome {
        parse::parse_line(ctx, line)
    }
}
