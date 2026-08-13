//! The canonical token model.
//!
//! # Why this type has private fields
//!
//! Anthropic and OpenAI both publish a field called `input_tokens` and mean
//! different things by it:
//!
//! | | Anthropic | OpenAI / Codex |
//! |---|---|---|
//! | cache reads | **excluded** from `input_tokens` | **included** in `input_tokens` |
//! | cache writes | **excluded** from `input_tokens` | additive, outside it |
//! | reasoning | not reported at all | **included** in `output_tokens` |
//!
//! Applying either convention universally corrupts totals *and* cost. Measured
//! against real data: charging Codex's `input_tokens` in full and its
//! `cached_input_tokens` again overstates a turn by **145%**; pricing
//! Anthropic's cache writes at a flat 1.25× rather than by TTL understates a
//! real session by **24%**.
//!
//! So [`TokenUsage`] holds a **disjoint partition** with **private fields**.
//! There is no `input_tokens` member for anyone to add `cache_read` to, and the
//! only ways to construct one are [`TokenUsage::from_anthropic`] and
//! [`TokenUsage::from_openai`], each of which encodes its provider's semantics
//! exactly once. The OpenAI constructor *cannot* forget to subtract the cached
//! portion, because subtraction is the only path that compiles.

use aum_contract::TokenBands;

use crate::native::{AnthropicUsage, OpenAiUsage};

/// A provider's usage, normalized into mutually exclusive buckets.
///
/// Because the buckets are disjoint, they sum correctly, stack correctly in a
/// chart, and price correctly — for every provider, with one formula.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// Fresh input at the full rate. Disjoint from every cache bucket.
    input_fresh: u64,
    /// Cache hits, billed at a discount.
    cache_read: u64,
    /// Cache writes, split by TTL because the multipliers differ.
    cache_write_5m: u64,
    cache_write_1h: u64,
    /// A cache write whose TTL the provider did not break down.
    cache_write_unspecified: u64,
    /// All output. Where a provider reports reasoning as a subset of output, it
    /// is already counted here and must not be added again.
    output_total: u64,
    /// Subset of `output_total`. `None` means the provider does not report it —
    /// never `Some(0)`, which would assert that no reasoning occurred.
    reasoning: Option<u64>,
}

/// A provider's numbers did not satisfy the provider's own stated semantics.
///
/// These are recorded and surfaced rather than silently repaired: a usage
/// object that fails its own invariants is evidence about the data, and quietly
/// coercing it would destroy exactly the signal this application exists to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NormalizeError {
    #[error(
        "cached_input_tokens ({cached}) exceeds input_tokens ({input}); \
         OpenAI semantics require cached to be a subset"
    )]
    CachedExceedsInput { input: u64, cached: u64 },

    #[error(
        "reasoning_output_tokens ({reasoning}) exceeds output_tokens ({output}); \
         reasoning must be a subset of output"
    )]
    ReasoningExceedsOutput { output: u64, reasoning: u64 },

    #[error(
        "cache_creation TTL split ({split_sum}) does not equal \
         cache_creation_input_tokens ({reported})"
    )]
    CacheTtlSplitMismatch { split_sum: u64, reported: u64 },

    #[error("provider total_tokens ({reported}) does not equal input + output ({computed})")]
    ProviderTotalMismatch { computed: u64, reported: u64 },
}

impl TokenUsage {
    /// Normalize Anthropic usage.
    ///
    /// No arithmetic is needed on the input side: Anthropic's buckets are
    /// already disjoint, so the fields map straight across. The only care
    /// required is the TTL split, which must be preserved because 5-minute and
    /// 1-hour cache writes price differently.
    ///
    /// `reasoning` is always `None`. Claude Code emits `thinking` content blocks
    /// but no count for them, and reporting `Some(0)` would be a different and
    /// unsupported claim.
    #[deny(clippy::arithmetic_side_effects, clippy::indexing_slicing)]
    pub fn from_anthropic(u: &AnthropicUsage) -> Result<Self, NormalizeError> {
        let cache_write_total = u.cache_creation_input_tokens.unwrap_or(0);

        let (w5, w1, unspecified) = match u.cache_creation {
            Some(c) => {
                let five = c.ephemeral_5m_input_tokens.unwrap_or(0);
                let hour = c.ephemeral_1h_input_tokens.unwrap_or(0);
                let split_sum = five.saturating_add(hour);

                // A mismatch means we would misprice the write. Surface it
                // rather than guessing which number to believe.
                if split_sum != cache_write_total {
                    return Err(NormalizeError::CacheTtlSplitMismatch {
                        split_sum,
                        reported: cache_write_total,
                    });
                }
                (five, hour, 0)
            }
            // No breakdown: keep the total, but in a bucket the pricing engine
            // knows it cannot price by TTL.
            None => (0, 0, cache_write_total),
        };

        Ok(Self {
            input_fresh: u.input_tokens.unwrap_or(0),
            cache_read: u.cache_read_input_tokens.unwrap_or(0),
            cache_write_5m: w5,
            cache_write_1h: w1,
            cache_write_unspecified: unspecified,
            output_total: u.output_tokens.unwrap_or(0),
            reasoning: None,
        })
    }

    /// Normalize OpenAI-shaped usage (Codex).
    ///
    /// The cached portion **must** be subtracted out of `input_tokens` to reach
    /// a disjoint partition. `checked_sub` is mandatory, not stylistic: an
    /// unchecked `u64` subtraction wraps to ~1.8×10¹⁹ in a release build, which
    /// is precisely where the bug would ship, and the resulting cost would be
    /// astronomically wrong while looking like a real number.
    #[deny(clippy::arithmetic_side_effects, clippy::indexing_slicing)]
    pub fn from_openai(u: &OpenAiUsage) -> Result<Self, NormalizeError> {
        let input = u.input_tokens.unwrap_or(0);
        let cached = u.cached_input_tokens.unwrap_or(0);
        let output = u.output_tokens.unwrap_or(0);
        let reasoning = u.reasoning_output_tokens.unwrap_or(0);

        let input_fresh = input
            .checked_sub(cached)
            .ok_or(NormalizeError::CachedExceedsInput { input, cached })?;

        if reasoning > output {
            return Err(NormalizeError::ReasoningExceedsOutput { output, reasoning });
        }

        Ok(Self {
            input_fresh,
            cache_read: cached,
            // OpenAI reports no TTL for cache writes, so it cannot be priced by
            // TTL and must not be silently assumed to be the cheaper tier.
            cache_write_5m: 0,
            cache_write_1h: 0,
            cache_write_unspecified: u.cache_write_input_tokens.unwrap_or(0),
            output_total: output,
            reasoning: Some(reasoning),
        })
    }

    /// Check the provider's own arithmetic without rejecting the row.
    ///
    /// Verified: one event in 21,784 fails this. The row is still usable — we
    /// simply record that the provider contradicted itself, which is a fact
    /// about the data worth surfacing.
    #[must_use]
    pub fn check_openai_total(u: &OpenAiUsage) -> Option<NormalizeError> {
        let reported = u.total_tokens?;
        let computed = u
            .input_tokens
            .unwrap_or(0)
            .saturating_add(u.output_tokens.unwrap_or(0));
        (computed != reported)
            .then_some(NormalizeError::ProviderTotalMismatch { computed, reported })
    }

    // ── accessors ────────────────────────────────────────────────────────────

    #[must_use]
    pub const fn input_fresh(&self) -> u64 {
        self.input_fresh
    }
    #[must_use]
    pub const fn cache_read(&self) -> u64 {
        self.cache_read
    }
    #[must_use]
    pub const fn cache_write_5m(&self) -> u64 {
        self.cache_write_5m
    }
    #[must_use]
    pub const fn cache_write_1h(&self) -> u64 {
        self.cache_write_1h
    }
    #[must_use]
    pub const fn cache_write_unspecified(&self) -> u64 {
        self.cache_write_unspecified
    }
    #[must_use]
    pub const fn output_total(&self) -> u64 {
        self.output_total
    }
    #[must_use]
    pub const fn reasoning(&self) -> Option<u64> {
        self.reasoning
    }

    #[must_use]
    pub const fn cache_write_total(&self) -> u64 {
        self.cache_write_5m
            .saturating_add(self.cache_write_1h)
            .saturating_add(self.cache_write_unspecified)
    }

    /// The only cross-provider-comparable input quantity, and what the UI labels
    /// "Input".
    #[must_use]
    pub const fn input_side_total(&self) -> u64 {
        self.input_fresh
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write_total())
    }

    /// Our total.
    ///
    /// Deliberately **not** the provider's `total_tokens`, which excludes cache
    /// writes: those are real tokens that were really billed.
    #[must_use]
    pub const fn grand_total(&self) -> u64 {
        self.input_side_total().saturating_add(self.output_total)
    }

    /// Component-wise addition, for aggregating a task.
    ///
    /// `reasoning` stays `None` unless at least one side reported it. When only
    /// some contributors report it the result is a floor, and the caller is
    /// responsible for marking the aggregate partial.
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
    /// repeating the whole `usage` object; where copies differ, the values are
    /// monotonically non-decreasing (an early line is flushed mid-stream).
    /// `max` is therefore both correct and **order-independent**, which is what
    /// makes re-ingesting a file after a crash provably safe.
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

impl From<TokenUsage> for TokenBands {
    fn from(u: TokenUsage) -> Self {
        Self {
            input_fresh: u.input_fresh,
            cache_read: u.cache_read,
            cache_write_5m: u.cache_write_5m,
            cache_write_1h: u.cache_write_1h,
            cache_write_unspecified: u.cache_write_unspecified,
            output_total: u.output_total,
            reasoning: u.reasoning,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::native::AnthropicCacheCreation;

    /// A real Claude Code request from the local corpus.
    ///
    /// Its raw `input_tokens` field reads **2**, while the request actually
    /// processed 54,497 input-side tokens. This single row is why the UI must
    /// never render a provider's raw input field.
    fn real_anthropic() -> AnthropicUsage {
        AnthropicUsage {
            input_tokens: Some(2),
            output_tokens: Some(4_452),
            cache_creation_input_tokens: Some(23_610),
            cache_read_input_tokens: Some(30_885),
            cache_creation: Some(AnthropicCacheCreation {
                ephemeral_5m_input_tokens: Some(0),
                ephemeral_1h_input_tokens: Some(23_610),
            }),
            ..Default::default()
        }
    }

    /// A real Codex turn from the local corpus.
    fn real_openai() -> OpenAiUsage {
        OpenAiUsage {
            input_tokens: Some(16_210),
            cached_input_tokens: Some(11_008),
            cache_write_input_tokens: Some(0),
            output_tokens: Some(164),
            reasoning_output_tokens: Some(35),
            total_tokens: Some(16_374),
        }
    }

    // ── I1–I4: Anthropic ─────────────────────────────────────────────────────

    #[test]
    fn i1_anthropic_input_field_maps_straight_to_fresh_without_arithmetic() {
        let u = TokenUsage::from_anthropic(&real_anthropic()).unwrap();
        assert_eq!(u.input_fresh(), 2);
    }

    #[test]
    fn i2_anthropic_input_side_total_is_the_sum_of_disjoint_buckets() {
        let u = TokenUsage::from_anthropic(&real_anthropic()).unwrap();
        // 2 fresh + 30,885 read + 23,610 written.
        assert_eq!(u.input_side_total(), 54_497);
        assert_eq!(u.grand_total(), 58_949);
    }

    #[test]
    fn i3_anthropic_ttl_split_is_preserved_because_it_changes_the_price() {
        let u = TokenUsage::from_anthropic(&real_anthropic()).unwrap();
        assert_eq!(u.cache_write_1h(), 23_610);
        assert_eq!(u.cache_write_5m(), 0);
        // Collapsing these into one bucket understates this request by ~24%.
        assert_eq!(u.cache_write_unspecified(), 0);
    }

    #[test]
    fn i4_anthropic_reasoning_is_none_never_some_zero() {
        let u = TokenUsage::from_anthropic(&real_anthropic()).unwrap();
        assert_eq!(u.reasoning(), None);
        assert_ne!(u.reasoning(), Some(0));
    }

    #[test]
    fn a_ttl_split_that_does_not_add_up_is_reported_not_guessed() {
        let mut native = real_anthropic();
        native.cache_creation = Some(AnthropicCacheCreation {
            ephemeral_5m_input_tokens: Some(1),
            ephemeral_1h_input_tokens: Some(1),
        });
        assert!(matches!(
            TokenUsage::from_anthropic(&native),
            Err(NormalizeError::CacheTtlSplitMismatch { .. })
        ));
    }

    #[test]
    fn a_missing_ttl_breakdown_keeps_the_tokens_in_an_unpriceable_bucket() {
        let mut native = real_anthropic();
        native.cache_creation = None;
        let u = TokenUsage::from_anthropic(&native).unwrap();
        // Not silently assumed to be the cheaper 5-minute tier.
        assert_eq!(u.cache_write_unspecified(), 23_610);
        assert_eq!(u.cache_write_5m(), 0);
        assert_eq!(u.cache_write_1h(), 0);
        assert_eq!(u.input_side_total(), 54_497);
    }

    // ── I5–I8: OpenAI ────────────────────────────────────────────────────────

    #[test]
    fn i5_openai_cached_tokens_are_subtracted_out_of_input() {
        let u = TokenUsage::from_openai(&real_openai()).unwrap();
        // 16,210 total input, of which 11,008 was cached.
        assert_eq!(u.input_fresh(), 5_202);
        assert_eq!(u.cache_read(), 11_008);
        // Not subtracting would overstate this turn's cost by 145%.
        assert_eq!(u.input_side_total(), 16_210);
    }

    #[test]
    fn i6_openai_reasoning_stays_inside_output_and_is_not_added_again() {
        let u = TokenUsage::from_openai(&real_openai()).unwrap();
        assert_eq!(u.output_total(), 164);
        assert_eq!(u.reasoning(), Some(35));
        assert!(u.reasoning().unwrap() <= u.output_total());
    }

    #[test]
    fn i7_our_total_includes_cache_writes_that_the_provider_total_omits() {
        let native = OpenAiUsage {
            cache_write_input_tokens: Some(1_000),
            ..real_openai()
        };
        let u = TokenUsage::from_openai(&native).unwrap();
        // Provider says 16,374; those 1,000 written tokens were still billed.
        assert_eq!(u.grand_total(), 16_374 + 1_000);
    }

    #[test]
    fn i8_cached_exceeding_input_is_an_error_not_a_wrapped_subtraction() {
        // In a release build an unchecked u64 subtraction here yields
        // ~1.8e19 tokens and a cost in the trillions.
        let native = OpenAiUsage {
            input_tokens: Some(10),
            cached_input_tokens: Some(20),
            ..Default::default()
        };
        assert_eq!(
            TokenUsage::from_openai(&native),
            Err(NormalizeError::CachedExceedsInput {
                input: 10,
                cached: 20
            })
        );
    }

    #[test]
    fn reasoning_exceeding_output_is_an_error() {
        let native = OpenAiUsage {
            output_tokens: Some(10),
            reasoning_output_tokens: Some(20),
            ..Default::default()
        };
        assert!(matches!(
            TokenUsage::from_openai(&native),
            Err(NormalizeError::ReasoningExceedsOutput { .. })
        ));
    }

    #[test]
    fn the_providers_own_arithmetic_is_checked_but_does_not_reject_the_row() {
        let native = OpenAiUsage {
            total_tokens: Some(99_999),
            ..real_openai()
        };
        // Usable...
        assert!(TokenUsage::from_openai(&native).is_ok());
        // ...but the contradiction is recorded.
        assert!(matches!(
            TokenUsage::check_openai_total(&native),
            Some(NormalizeError::ProviderTotalMismatch { .. })
        ));
        assert!(TokenUsage::check_openai_total(&real_openai()).is_none());
    }

    // ── I9–I10: cross-provider ───────────────────────────────────────────────

    #[test]
    fn i9_the_two_providers_become_comparable_only_after_normalization() {
        let claude = TokenUsage::from_anthropic(&real_anthropic()).unwrap();
        let codex = TokenUsage::from_openai(&real_openai()).unwrap();

        // Raw fields invite the wrong conclusion: 2 vs 16,210 suggests the
        // Claude request was thousands of times smaller.
        assert_eq!(real_anthropic().input_tokens.unwrap(), 2);
        assert_eq!(real_openai().input_tokens.unwrap(), 16_210);

        // Normalized, the truth is the other way round.
        assert!(claude.input_side_total() > codex.input_side_total());
        assert_eq!(claude.input_side_total(), 54_497);
        assert_eq!(codex.input_side_total(), 16_210);
    }

    #[test]
    fn i10_normalization_is_deterministic() {
        let a = TokenUsage::from_anthropic(&real_anthropic()).unwrap();
        let b = TokenUsage::from_anthropic(&real_anthropic()).unwrap();
        assert_eq!(a, b);
    }

    // ── aggregation ──────────────────────────────────────────────────────────

    #[test]
    fn summing_a_claude_and_a_codex_request_keeps_reasoning_as_a_floor() {
        let claude = TokenUsage::from_anthropic(&real_anthropic()).unwrap();
        let codex = TokenUsage::from_openai(&real_openai()).unwrap();
        let total = claude.merge_add(codex);

        assert_eq!(total.input_side_total(), 54_497 + 16_210);
        // Claude contributed an unknown amount of reasoning, not zero, so this
        // is a lower bound that the aggregate must be marked partial about.
        assert_eq!(total.reasoning(), Some(35));
    }

    #[test]
    fn summing_two_claude_requests_leaves_reasoning_unknown() {
        let a = TokenUsage::from_anthropic(&real_anthropic()).unwrap();
        let b = TokenUsage::from_anthropic(&real_anthropic()).unwrap();
        assert_eq!(a.merge_add(b).reasoning(), None);
    }

    #[test]
    fn merge_max_is_order_independent_and_idempotent() {
        // Two copies of one Claude response: the first flushed mid-stream with a
        // preliminary output count, the second final.
        let early = TokenUsage::from_anthropic(&AnthropicUsage {
            input_tokens: Some(5),
            output_tokens: Some(1),
            ..Default::default()
        })
        .unwrap();
        let final_ = TokenUsage::from_anthropic(&AnthropicUsage {
            input_tokens: Some(5),
            output_tokens: Some(378),
            ..Default::default()
        })
        .unwrap();

        assert_eq!(early.merge_max(final_), final_.merge_max(early));
        assert_eq!(early.merge_max(final_).output_total(), 378);
        // Re-ingesting the same file must converge, not accumulate.
        assert_eq!(final_.merge_max(final_), final_);
    }

    #[test]
    fn merging_duplicates_does_not_multiply_them() {
        // The verified failure mode: one API response written as 8 JSONL lines,
        // each repeating the whole usage object. Summing yields 8x.
        let one = TokenUsage::from_anthropic(&real_anthropic()).unwrap();
        let deduped = (0..8).fold(TokenUsage::default(), |acc, _| acc.merge_max(one));
        assert_eq!(deduped, one);

        let naive = (0..8).fold(TokenUsage::default(), |acc, _| acc.merge_add(one));
        assert_eq!(naive.output_total(), 4_452 * 8);
    }

    #[test]
    fn converting_to_wire_bands_preserves_every_bucket() {
        let u = TokenUsage::from_anthropic(&real_anthropic()).unwrap();
        let bands: TokenBands = u.into();
        assert_eq!(bands.input_side_total(), u.input_side_total());
        assert_eq!(bands.reasoning, None);
    }
}
