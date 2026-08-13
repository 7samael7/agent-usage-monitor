//! Provider-native usage, captured verbatim.
//!
//! This layer is the audit trail. The application is a measurement instrument,
//! so it must be able to prove any number it displays by pointing at the exact
//! bytes it came from — which means keeping the provider's own fields, in the
//! provider's own semantics, untouched.
//!
//! Every field is `Option` and nothing is `deny_unknown_fields`. These are
//! undocumented on-disk formats belonging to tools that ship weekly; a parser
//! that refuses an unfamiliar key would start silently losing data the first
//! time either vendor adds one.

use serde::{Deserialize, Serialize};

/// Anthropic's `usage` object, as relayed verbatim by Claude Code.
///
/// **Semantics:** `input_tokens` **excludes** `cache_creation_input_tokens` and
/// `cache_read_input_tokens`. The three are disjoint and billed at different
/// rates. This is the opposite of OpenAI's convention.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicUsage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    /// TTL split of the cache write. Required for correct pricing: 5-minute and
    /// 1-hour writes carry different multipliers, and real sessions are
    /// dominated by 1-hour writes.
    #[serde(default)]
    pub cache_creation: Option<AnthropicCacheCreation>,
    #[serde(default)]
    pub server_tool_use: Option<ServerToolUse>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub speed: Option<String>,
    #[serde(default)]
    pub inference_geo: Option<String>,
    /// A **breakdown of this same message**, never an addend.
    ///
    /// Verified across a full session: `sum(iterations[].output_tokens)` equals
    /// the top-level `output_tokens` for 100% of rows. Adding them doubles
    /// every message.
    #[serde(default)]
    pub iterations: Option<Vec<AnthropicIteration>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicCacheCreation {
    #[serde(default)]
    pub ephemeral_5m_input_tokens: Option<u64>,
    #[serde(default)]
    pub ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerToolUse {
    #[serde(default)]
    pub web_search_requests: Option<u64>,
    #[serde(default)]
    pub web_fetch_requests: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicIteration {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
}

/// OpenAI-shaped usage, as reported by Codex in a `token_count` event.
///
/// **Semantics:** `cached_input_tokens` is a **subset of** `input_tokens`, and
/// `reasoning_output_tokens` is a **subset of** `output_tokens`.
/// `total_tokens == input_tokens + output_tokens`, with cache writes excluded.
/// Verified arithmetically across 21,784 real events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiUsage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// Subset of `input_tokens`.
    #[serde(default)]
    pub cached_input_tokens: Option<u64>,
    /// Additive, outside `input_tokens`. Empirically always 0 in the local
    /// corpus, so the pricing path for it exists but is unvalidated against
    /// real data — flagged rather than assumed correct.
    #[serde(default)]
    pub cache_write_input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    /// Subset of `output_tokens`. Billed at the output rate, not separately.
    #[serde(default)]
    pub reasoning_output_tokens: Option<u64>,
    /// The provider's own total. Cross-checked, never trusted blindly: one
    /// event in 21,784 failed the provider's own arithmetic.
    #[serde(default)]
    pub total_tokens: Option<u64>,
}

impl OpenAiUsage {
    /// Component-wise difference, for turning a cumulative counter into a delta.
    ///
    /// Saturating rather than wrapping: a counter that appears to go backwards
    /// is an anomaly to record, not a reason to emit a 1.8e19-token request.
    #[must_use]
    pub fn saturating_delta(self, previous: Self) -> Self {
        fn sub(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.saturating_sub(b)),
                (Some(a), None) => Some(a),
                (None, _) => None,
            }
        }
        Self {
            input_tokens: sub(self.input_tokens, previous.input_tokens),
            cached_input_tokens: sub(self.cached_input_tokens, previous.cached_input_tokens),
            cache_write_input_tokens: sub(
                self.cache_write_input_tokens,
                previous.cache_write_input_tokens,
            ),
            output_tokens: sub(self.output_tokens, previous.output_tokens),
            reasoning_output_tokens: sub(
                self.reasoning_output_tokens,
                previous.reasoning_output_tokens,
            ),
            total_tokens: sub(self.total_tokens, previous.total_tokens),
        }
    }

    /// True when every component is zero — i.e. the cumulative counter did not
    /// move, which is how a replayed event presents.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.input_tokens.unwrap_or(0) == 0
            && self.cached_input_tokens.unwrap_or(0) == 0
            && self.cache_write_input_tokens.unwrap_or(0) == 0
            && self.output_tokens.unwrap_or(0) == 0
            && self.reasoning_output_tokens.unwrap_or(0) == 0
    }
}

/// Which provider authored a usage object. Determines how it is normalized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum NativeUsage {
    Anthropic(AnthropicUsage),
    OpenAi(OpenAiUsage),
}
