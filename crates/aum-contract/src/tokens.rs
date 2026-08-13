//! Normalized token usage on the wire.
//!
//! Anthropic and OpenAI both publish a field called `input_tokens` and they mean
//! different things:
//!
//! * **Anthropic** — `input_tokens` *excludes* cache creation and cache read.
//!   The three are disjoint and billed at different rates.
//! * **OpenAI/Codex** — `cached_input_tokens` is a *subset of* `input_tokens`,
//!   and `reasoning_output_tokens` is a *subset of* `output_tokens`.
//!
//! Treating either convention as universal corrupts both totals and cost:
//! charging Codex's `input_tokens` in full *and* its `cached_input_tokens` again
//! overstates that turn by 145%.
//!
//! The wire type below is therefore a **disjoint partition**. There is no field
//! called `input_tokens`, so there is nothing for a consumer to naively add
//! `cache_read` to. Producing one is the job of `aum_domain::TokenUsage`, whose
//! constructors encode each provider's semantics exactly once.

use serde::{Deserialize, Serialize};

/// Token counts in mutually exclusive buckets.
///
/// Because the buckets are disjoint, stacking them in a chart is arithmetically
/// valid for every provider, and `cache_read / input_side_total` is a single
/// cache-hit-rate formula that means the same thing everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TokenBands {
    /// Fresh input, billed at the full input rate. Disjoint from every cache bucket.
    pub input_fresh: u64,
    /// Cache hits, billed at a discount.
    pub cache_read: u64,
    /// Cache writes with a 5-minute TTL.
    pub cache_write_5m: u64,
    /// Cache writes with a 1-hour TTL. Priced differently from 5-minute writes;
    /// collapsing the two understates real Claude Code sessions by ~24%.
    pub cache_write_1h: u64,
    /// A cache write whose TTL the provider did not break down.
    pub cache_write_unspecified: u64,
    /// All output tokens. Where a provider reports reasoning as a subset of
    /// output, it is *already included here* and must not be added again.
    pub output_total: u64,
    /// Reasoning tokens, a subset of `output_total`.
    ///
    /// `None` means the provider does not report them — which is the case for
    /// Claude Code. It must never be serialized as `0`: zero asserts that no
    /// reasoning happened, which is a different and unsupported claim.
    pub reasoning: Option<u64>,
}

impl TokenBands {
    #[must_use]
    pub const fn cache_write_total(&self) -> u64 {
        self.cache_write_5m
            .saturating_add(self.cache_write_1h)
            .saturating_add(self.cache_write_unspecified)
    }

    /// The only cross-provider-comparable input quantity, and what the UI labels
    /// "Input".
    ///
    /// Rendering a provider's raw `input_tokens` instead would show `2` for a
    /// real Claude request that processed 54,497 input-side tokens, next to
    /// `16,210` for a Codex one — inviting the reader to conclude the Claude
    /// request was thousands of times smaller when it was in fact larger.
    #[must_use]
    pub const fn input_side_total(&self) -> u64 {
        self.input_fresh
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write_total())
    }

    #[must_use]
    pub const fn grand_total(&self) -> u64 {
        self.input_side_total().saturating_add(self.output_total)
    }

    /// Cache hit rate over the input side, `None` when there was no input at all
    /// (rather than a misleading 0%).
    #[must_use]
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let denom = self.input_side_total();
        if denom == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        Some(self.cache_read as f64 / denom as f64)
    }

    /// Component-wise addition for aggregation.
    ///
    /// `reasoning` deliberately stays `None` unless at least one side reported
    /// it; a sum of unknowns is not zero. When only one side reports, the result
    /// is a floor, and the caller is responsible for marking the aggregate
    /// `Partial`.
    #[must_use]
    pub fn merge_add(self, other: Self) -> Self {
        Self {
            input_fresh: self.input_fresh.saturating_add(other.input_fresh),
            cache_read: self.cache_read.saturating_add(other.cache_read),
            cache_write_5m: self.cache_write_5m.saturating_add(other.cache_write_5m),
            cache_write_1h: self.cache_write_1h.saturating_add(other.cache_write_1h),
            cache_write_unspecified: self
                .cache_write_unspecified
                .saturating_add(other.cache_write_unspecified),
            output_total: self.output_total.saturating_add(other.output_total),
            reasoning: match (self.reasoning, other.reasoning) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
            },
        }
    }

    /// Component-wise maximum.
    ///
    /// Claude Code writes one API response as several JSONL lines, each
    /// repeating the whole `usage` object; where the copies differ, the values
    /// are monotonically non-decreasing (an early line is flushed mid-stream).
    /// Taking the max is therefore correct *and* order-independent, which is
    /// what makes re-ingest after a crash provably safe.
    #[must_use]
    pub fn merge_max(self, other: Self) -> Self {
        Self {
            input_fresh: self.input_fresh.max(other.input_fresh),
            cache_read: self.cache_read.max(other.cache_read),
            cache_write_5m: self.cache_write_5m.max(other.cache_write_5m),
            cache_write_1h: self.cache_write_1h.max(other.cache_write_1h),
            cache_write_unspecified: self
                .cache_write_unspecified
                .max(other.cache_write_unspecified),
            output_total: self.output_total.max(other.output_total),
            reasoning: match (self.reasoning, other.reasoning) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, b) => b,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn input_side_total_is_the_sum_of_disjoint_buckets() {
        let b = TokenBands {
            input_fresh: 2,
            cache_read: 30_885,
            cache_write_1h: 23_610,
            output_total: 4_452,
            ..Default::default()
        };
        // The real Claude request whose raw `input_tokens` field reads 2.
        assert_eq!(b.input_side_total(), 54_497);
        assert_eq!(b.grand_total(), 58_949);
    }

    #[test]
    fn reasoning_stays_none_when_neither_side_reports_it() {
        let a = TokenBands {
            output_total: 10,
            reasoning: None,
            ..Default::default()
        };
        let b = TokenBands {
            output_total: 20,
            reasoning: None,
            ..Default::default()
        };
        assert_eq!(a.merge_add(b).reasoning, None);
    }

    #[test]
    fn reasoning_is_a_floor_when_only_one_side_reports_it() {
        let claude = TokenBands {
            output_total: 100,
            reasoning: None,
            ..Default::default()
        };
        let codex = TokenBands {
            output_total: 50,
            reasoning: Some(35),
            ..Default::default()
        };
        assert_eq!(claude.merge_add(codex).reasoning, Some(35));
    }

    #[test]
    fn merge_max_is_order_independent() {
        // Two copies of one Claude response, the first flushed mid-stream.
        let early = TokenBands {
            input_fresh: 5,
            output_total: 1,
            ..Default::default()
        };
        let final_ = TokenBands {
            input_fresh: 5,
            output_total: 378,
            ..Default::default()
        };
        assert_eq!(early.merge_max(final_), final_.merge_max(early));
        assert_eq!(early.merge_max(final_).output_total, 378);
    }

    #[test]
    fn merge_max_is_idempotent() {
        let b = TokenBands {
            input_fresh: 5,
            output_total: 378,
            ..Default::default()
        };
        assert_eq!(b.merge_max(b), b);
    }

    #[test]
    fn cache_hit_rate_is_none_rather_than_zero_when_there_was_no_input() {
        assert_eq!(TokenBands::default().cache_hit_rate(), None);
    }

    #[test]
    fn reasoning_absent_serializes_as_null_not_zero() {
        let b = TokenBands {
            output_total: 100,
            reasoning: None,
            ..Default::default()
        };
        let json = serde_json::to_value(b).unwrap();
        assert_eq!(json["reasoning"], serde_json::Value::Null);
    }
}
