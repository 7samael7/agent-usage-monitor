//! Turning Codex's cumulative counter into per-request rows.
//!
//! Codex reports two things after every turn: `total_token_usage`, a running
//! total for the session, and `last_token_usage`, the most recent call. Neither
//! is correct on its own, and the error runs in opposite directions.
//!
//! Measured across all 21,784 usage-bearing events on this machine:
//!
//! | transition | count | what it is |
//! |---|---:|---|
//! | cumulative advanced | 21,341 | an ordinary turn |
//! | advanced by zero, `last` unchanged | 181 | the same event emitted twice |
//! | advanced by zero, `last` changed | 203 | a call billed but left off the ledger |
//! | cumulative went backwards | 1 | the session counter was rebased |
//!
//! Summing `last_token_usage` therefore **over**-counts by ~0.89%, because it
//! counts the 181 duplicates. Trusting the final `total_token_usage` **under**-
//! counts by ~0.98%, because it misses the 203 compaction calls — which are
//! real API calls, really billed, that the provider excludes from its own
//! running total. Every one of those 203 coincides with a `context_compacted`
//! event.
//!
//! So the cumulative counter is authoritative for ordinary turns, `last` is the
//! cross-check, and the disagreement between them is itself the signal that
//! identifies the off-ledger calls.

use aum_domain::OpenAiUsage;

/// What one `token_count` event means.
#[derive(Debug, Clone, PartialEq)]
pub enum Reconciled {
    /// An ordinary turn. The delta is the difference in the cumulative counter.
    Turn { usage: OpenAiUsage },

    /// The identical event again. Contributes nothing.
    ///
    /// This is what makes re-ingest safe: a replayed event carries an identical
    /// cumulative vector, so it collapses structurally rather than by a special
    /// case, and the same logic protects against re-reading a file after a crash.
    Replay,

    /// A call the provider billed but left out of its own running total.
    ///
    /// Always a compaction summarization. Recorded as a distinct request kind so
    /// it is visible in the timeline rather than silently folded into a turn.
    OffLedger { usage: OpenAiUsage },

    /// The cumulative counter moved backwards — the session was rebased or
    /// forked. Start a new epoch rather than emitting a negative delta.
    EpochReset {
        usage: OpenAiUsage,
        from_total: u64,
        to_total: u64,
    },

    /// The event carried no token information at all (`info: null`).
    ///
    /// 18 such events exist in the corpus; they carry rate-limit status only.
    /// Treating them as a zero-token turn would inflate the request count.
    NoTokenData,
}

/// Per-session reconciliation state.
///
/// Persisted with the file cursor, so a restart resumes rather than
/// re-deriving. Even if it is lost, re-reading from the beginning is safe:
/// every row's identity is its cumulative watermark, so everything at or below
/// the watermark collapses to [`Reconciled::Replay`].
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Watermark {
    pub cumulative: Option<OpenAiUsage>,
    pub last: Option<OpenAiUsage>,
    /// Incremented whenever the counter is rebased, so keys stay unique across
    /// a fork.
    pub epoch: u32,
    /// Running sum of everything emitted, for the reconciliation check.
    pub emitted_total: u64,
}

impl Watermark {
    /// Advance by one event.
    ///
    /// Not `#[must_use]`: the primary effect is advancing the watermark, and
    /// callers that only need the state moved on legitimately ignore the
    /// outcome.
    pub fn observe(
        &mut self,
        cumulative: Option<OpenAiUsage>,
        last: Option<OpenAiUsage>,
    ) -> Reconciled {
        let Some(cumulative) = cumulative else {
            return Reconciled::NoTokenData;
        };

        let Some(previous) = self.cumulative else {
            // First event of a session: the cumulative *is* the delta.
            self.cumulative = Some(cumulative);
            self.last = last;
            self.emitted_total = self.emitted_total.saturating_add(total_of(&cumulative));
            return Reconciled::Turn { usage: cumulative };
        };

        let now_total = total_of(&cumulative);
        let then_total = total_of(&previous);

        if now_total < then_total {
            self.epoch = self.epoch.saturating_add(1);
            self.cumulative = Some(cumulative);
            let emitted = last.unwrap_or(cumulative);
            self.last = last;
            self.emitted_total = self.emitted_total.saturating_add(total_of(&emitted));
            return Reconciled::EpochReset {
                usage: emitted,
                from_total: then_total,
                to_total: now_total,
            };
        }

        if now_total == then_total {
            // The ledger did not move. Either nothing happened and this event is
            // a duplicate, or a call happened that the ledger excludes.
            if last == self.last {
                return Reconciled::Replay;
            }
            let usage = last.unwrap_or_default();
            self.last = last;
            self.emitted_total = self.emitted_total.saturating_add(total_of(&usage));
            return Reconciled::OffLedger { usage };
        }

        let delta = cumulative.saturating_delta(previous);
        self.cumulative = Some(cumulative);
        self.last = last;
        self.emitted_total = self.emitted_total.saturating_add(total_of(&delta));
        Reconciled::Turn { usage: delta }
    }

    /// Does the sum of what we emitted match the provider's own running total?
    ///
    /// A mismatch is expected and benign in one direction — off-ledger calls are
    /// deliberately counted by us and not by the provider — so this reports the
    /// difference rather than asserting equality.
    #[must_use]
    pub fn reconciliation_gap(&self) -> i64 {
        let provider = self.cumulative.map_or(0, |c| total_of(&c));
        i64::try_from(self.emitted_total)
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(provider).unwrap_or(i64::MAX))
    }
}

/// The provider's own total excludes cache writes, so this uses it directly
/// rather than recomputing: the comparison must be like-for-like with the field
/// the counter actually advances.
fn total_of(u: &OpenAiUsage) -> u64 {
    u.total_tokens.unwrap_or_else(|| {
        u.input_tokens
            .unwrap_or(0)
            .saturating_add(u.output_tokens.unwrap_or(0))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn usage(input: u64, cached: u64, output: u64, reasoning: u64) -> OpenAiUsage {
        OpenAiUsage {
            input_tokens: Some(input),
            cached_input_tokens: Some(cached),
            cache_write_input_tokens: Some(0),
            output_tokens: Some(output),
            reasoning_output_tokens: Some(reasoning),
            total_tokens: Some(input + output),
        }
    }

    #[test]
    fn the_first_event_is_its_own_delta() {
        let mut w = Watermark::default();
        let first = usage(18_517, 12_160, 328, 106);
        assert_eq!(
            w.observe(Some(first), Some(first)),
            Reconciled::Turn { usage: first }
        );
    }

    #[test]
    fn an_ordinary_turn_emits_the_difference_not_the_running_total() {
        let mut w = Watermark::default();
        let a = usage(1_000, 0, 100, 0);
        let b = usage(3_000, 500, 250, 50);

        w.observe(Some(a), Some(a));
        let Reconciled::Turn { usage: delta } =
            w.observe(Some(b), Some(usage(2_000, 500, 150, 50)))
        else {
            panic!("expected a turn")
        };

        assert_eq!(delta.input_tokens, Some(2_000));
        assert_eq!(delta.output_tokens, Some(150));
    }

    #[test]
    fn a_verbatim_duplicate_contributes_nothing() {
        // 181 of these exist in the corpus. Summing `last` counts them and
        // over-reports the session by ~0.89%.
        let mut w = Watermark::default();
        let a = usage(1_000, 0, 100, 0);
        w.observe(Some(a), Some(a));
        assert_eq!(w.observe(Some(a), Some(a)), Reconciled::Replay);
        assert_eq!(w.observe(Some(a), Some(a)), Reconciled::Replay);
    }

    #[test]
    fn a_call_the_ledger_excludes_is_still_counted() {
        // 203 of these exist, every one alongside a compaction. Trusting the
        // provider's cumulative alone under-reports by ~0.98%.
        let mut w = Watermark::default();
        let cum = usage(24_873_976, 0, 0, 0);
        w.observe(Some(cum), Some(usage(210_476, 0, 0, 0)));

        let compaction_call = usage(15_683, 0, 0, 0);
        let outcome = w.observe(Some(cum), Some(compaction_call));

        assert_eq!(
            outcome,
            Reconciled::OffLedger {
                usage: compaction_call
            },
            "a billed call must not vanish just because the provider omits it"
        );
    }

    #[test]
    fn the_duplicate_that_follows_an_off_ledger_call_is_still_a_duplicate() {
        // The verified real sequence: an off-ledger call, then the identical
        // event emitted again. Only the first may count.
        let mut w = Watermark::default();
        let cum = usage(24_873_976, 0, 0, 0);
        w.observe(Some(cum), Some(usage(210_476, 0, 0, 0)));

        let call = usage(15_683, 0, 0, 0);
        assert!(matches!(
            w.observe(Some(cum), Some(call)),
            Reconciled::OffLedger { .. }
        ));
        assert_eq!(w.observe(Some(cum), Some(call)), Reconciled::Replay);
    }

    #[test]
    fn compaction_does_not_rebase_the_counter() {
        // The counter is verified never to decrease across a compaction. An
        // implementation that re-baselines here re-counts the entire remainder
        // of the session — on the real file, tens of millions of phantom tokens.
        let mut w = Watermark::default();
        let before = usage(24_873_976, 0, 0, 0);
        w.observe(Some(before), Some(usage(210_476, 0, 0, 0)));

        // compaction happens here in the event stream
        let after = usage(24_899_966, 0, 25_990, 0);
        let Reconciled::Turn { usage: delta } =
            w.observe(Some(after), Some(usage(25_990, 0, 0, 0)))
        else {
            panic!("expected an ordinary turn after compaction")
        };
        assert_eq!(delta.input_tokens, Some(25_990));
    }

    #[test]
    fn a_backwards_counter_starts_a_new_epoch_rather_than_a_negative_delta() {
        // One real occurrence: 26,836,493 -> 353,400.
        let mut w = Watermark::default();
        w.observe(Some(usage(26_836_493, 0, 0, 0)), Some(usage(100, 0, 0, 0)));

        let outcome = w.observe(Some(usage(353_400, 0, 0, 0)), Some(usage(400, 0, 0, 0)));
        let Reconciled::EpochReset { usage: emitted, .. } = outcome else {
            panic!("expected an epoch reset, got {outcome:?}")
        };
        // Emits the reported call, never a negative or wrapped delta.
        assert_eq!(emitted.input_tokens, Some(400));
        assert_eq!(w.epoch, 1);
    }

    #[test]
    fn an_event_with_no_token_data_is_not_a_zero_token_turn() {
        // 18 real events carry rate limits and `info: null`. Counting them as
        // turns would inflate the request count with requests that never
        // happened.
        let mut w = Watermark::default();
        assert_eq!(w.observe(None, None), Reconciled::NoTokenData);
        assert_eq!(w.cumulative, None);
    }

    #[test]
    fn replaying_an_entire_session_from_the_start_is_idempotent() {
        // The property that makes crash recovery safe without persisted state.
        let events: Vec<OpenAiUsage> = (1..=10).map(|i| usage(i * 1_000, 0, i * 10, 0)).collect();

        let run = |events: &[OpenAiUsage]| {
            let mut w = Watermark::default();
            let mut total = 0_u64;
            for e in events {
                if let Reconciled::Turn { usage } = w.observe(Some(*e), Some(*e)) {
                    total += usage.input_tokens.unwrap_or(0);
                }
            }
            total
        };

        let once = run(&events);
        // Every event seen twice, as a re-read would present them.
        let doubled: Vec<OpenAiUsage> = events.iter().flat_map(|e| [*e, *e]).collect();
        assert_eq!(run(&doubled), once, "re-reading must not double a session");
    }

    #[test]
    fn deltas_sum_back_to_the_providers_own_total() {
        let mut w = Watermark::default();
        let mut sum = 0_u64;
        for i in 1..=20_u64 {
            let cum = usage(i * 100, 0, i * 10, 0);
            if let Reconciled::Turn { usage } = w.observe(Some(cum), Some(cum)) {
                sum += usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0);
            }
        }
        assert_eq!(sum, 20 * 100 + 20 * 10);
        assert_eq!(w.reconciliation_gap(), 0);
    }

    #[test]
    fn the_reconciliation_gap_reports_off_ledger_calls_rather_than_hiding_them() {
        let mut w = Watermark::default();
        let cum = usage(1_000, 0, 100, 0);
        w.observe(Some(cum), Some(cum));
        w.observe(Some(cum), Some(usage(50, 0, 5, 0)));

        // We counted 55 tokens the provider's own total does not include. That
        // is a real difference and should be visible, not silently reconciled.
        assert_eq!(w.reconciliation_gap(), 55);
    }
}
