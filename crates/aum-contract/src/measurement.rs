//! Measurement provenance.
//!
//! Every number this application displays carries the story of where it came
//! from. The rule the whole product rests on:
//!
//! > A number is never presented as more certain than it is. Missing data is
//! > *unavailable*, never zero, and never quietly replaced by an estimate.
//!
//! Classification is by **authorship of the number** — who computed it — not by
//! the medium it travelled through. Claude Code's transcript `usage` block is
//! [`MeasurementSource::ProviderReported`] because it is Anthropic's own object
//! relayed verbatim (it carries `service_tier` and `inference_geo`, which the
//! CLI cannot compute). The cost figure in the same file is
//! [`MeasurementSource::ApplicationTelemetry`], because Anthropic never sent a
//! dollar amount for subscription usage — the CLI derived it.

use serde::{Deserialize, Serialize};

/// Who authored a measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementSource {
    /// The provider's own serving/billing infrastructure produced this number and
    /// it reached us without recomputation.
    ProviderReported,
    /// Read by us off the client↔provider wire (our proxy, on traffic the user
    /// explicitly routed through it). Provider-authored, observed at transport.
    ProtocolMetadata,
    /// Computed by the *agent application*, not the provider.
    ApplicationTelemetry,
    /// We ran the model's real tokenizer over text we legitimately hold.
    /// Deterministic, but blind to system prompts and tool schemas.
    TokenizerCalculated,
    /// Heuristic approximation. Only ever produced on explicit opt-in.
    Estimated,
}

impl MeasurementSource {
    /// How this source is described to a human.
    #[must_use]
    pub const fn display_kind(self) -> DisplayKind {
        match self {
            Self::ProviderReported | Self::ProtocolMetadata => DisplayKind::Exact,
            Self::ApplicationTelemetry | Self::TokenizerCalculated => DisplayKind::Calculated,
            Self::Estimated => DisplayKind::Estimated,
        }
    }
}

/// The four words the UI is allowed to use about a number's certainty, plus
/// `Partial` for aggregates that are missing some of their inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DisplayKind {
    Exact,
    Calculated,
    Estimated,
    /// Some contributing measurements are missing. Rendered with `≥`.
    Partial,
    Unavailable,
}

/// Why a measurement could not be produced. Surfaced to the user verbatim —
/// "unavailable" without a reason is not much better than a wrong number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnavailableReason {
    /// The application exposes no usage data at all (e.g. Claude Desktop).
    NoTelemetry { detail: String },
    /// This provider does not report this particular field (e.g. Claude Code
    /// reports no reasoning-token count).
    NotReportedByProvider { field: String, detail: String },
    /// A request happened but failed before any usage was reported.
    RequestFailed { detail: String },
    /// We have tokens but no price for this model.
    NoPricingForModel { model_id: String },
    /// The model was not known at ingest time, so cost cannot be computed.
    ModelUnknown { detail: String },
    /// Billed under a subscription; a per-token charge does not exist.
    SubscriptionBilled { plan: String },
    /// Requires a capture level the user has not enabled.
    RequiresCaptureLevel { level: String, detail: String },
}

impl UnavailableReason {
    /// A single sentence for a tooltip or `aria-label`.
    #[must_use]
    pub fn sentence(&self) -> String {
        match self {
            Self::NoTelemetry { detail }
            | Self::RequestFailed { detail }
            | Self::ModelUnknown { detail } => detail.clone(),
            Self::NotReportedByProvider { detail, .. } => detail.clone(),
            Self::NoPricingForModel { model_id } => {
                format!("No price is configured for model `{model_id}`.")
            }
            Self::SubscriptionBilled { plan } => {
                format!("Billed under the {plan} subscription, not per token.")
            }
            Self::RequiresCaptureLevel { level, detail } => {
                format!("{detail} Enable {level} to measure this.")
            }
        }
    }
}

/// The certainty attached to a value.
///
/// `Unavailable` is the only variant that may accompany a missing value, and a
/// present value may never be `Unavailable`. [`Measured`] enforces both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Accuracy {
    Exact {
        source: MeasurementSource,
    },
    Calculated {
        source: MeasurementSource,
    },
    Estimated {
        source: MeasurementSource,
    },
    /// An aggregate where some contributors reported and some did not. The value
    /// is a floor over `measured` of `total` inputs, and is rendered with `≥`.
    Partial {
        measured: u32,
        total: u32,
    },
    Unavailable {
        reason: UnavailableReason,
    },
}

impl Accuracy {
    #[must_use]
    pub const fn display_kind(&self) -> DisplayKind {
        match self {
            Self::Exact { .. } => DisplayKind::Exact,
            Self::Calculated { .. } => DisplayKind::Calculated,
            Self::Estimated { .. } => DisplayKind::Estimated,
            Self::Partial { .. } => DisplayKind::Partial,
            Self::Unavailable { .. } => DisplayKind::Unavailable,
        }
    }

    #[must_use]
    pub const fn is_available(&self) -> bool {
        !matches!(self, Self::Unavailable { .. })
    }

    /// Ordering used when folding many measurements into one: the weakest
    /// certainty present wins. `Unavailable` is strongest-losing of all, because
    /// a total that silently omits an input is worse than one labelled `Partial`.
    #[must_use]
    const fn strength(&self) -> u8 {
        match self {
            Self::Exact { .. } => 4,
            Self::Calculated { .. } => 3,
            Self::Estimated { .. } => 2,
            Self::Partial { .. } => 1,
            Self::Unavailable { .. } => 0,
        }
    }
}

/// A value together with how certain it is.
///
/// The invariant — `value.is_some()` exactly when the accuracy is not
/// `Unavailable` — is maintained by the constructors, and there is no public
/// field-literal path around them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Measured<T> {
    /// `None` if and only if `accuracy` is `Unavailable`.
    pub value: Option<T>,
    pub accuracy: Accuracy,
}

impl<T> Measured<T> {
    /// A number the provider itself reported.
    #[must_use]
    pub const fn exact(value: T, source: MeasurementSource) -> Self {
        Self {
            value: Some(value),
            accuracy: Accuracy::Exact { source },
        }
    }

    /// A number we derived from data we hold.
    #[must_use]
    pub const fn calculated(value: T, source: MeasurementSource) -> Self {
        Self {
            value: Some(value),
            accuracy: Accuracy::Calculated { source },
        }
    }

    #[must_use]
    pub const fn estimated(value: T) -> Self {
        Self {
            value: Some(value),
            accuracy: Accuracy::Estimated {
                source: MeasurementSource::Estimated,
            },
        }
    }

    /// A floor: `measured` of `total` contributors reported a value.
    #[must_use]
    pub const fn partial(value: T, measured: u32, total: u32) -> Self {
        Self {
            value: Some(value),
            accuracy: Accuracy::Partial { measured, total },
        }
    }

    /// No number. There is deliberately no way to supply one alongside a reason.
    #[must_use]
    pub const fn unavailable(reason: UnavailableReason) -> Self {
        Self {
            value: None,
            accuracy: Accuracy::Unavailable { reason },
        }
    }

    /// Build from a source and an `Option`, classifying automatically.
    ///
    /// This is the bridge used by adapters: a provider field that is absent
    /// becomes `Unavailable` with a reason, never `Some(0)`.
    #[must_use]
    pub fn from_option(
        value: Option<T>,
        source: MeasurementSource,
        absent: UnavailableReason,
    ) -> Self {
        match value {
            Some(v) => match source.display_kind() {
                DisplayKind::Exact => Self::exact(v, source),
                DisplayKind::Estimated => Self {
                    value: Some(v),
                    accuracy: Accuracy::Estimated { source },
                },
                _ => Self::calculated(v, source),
            },
            None => Self::unavailable(absent),
        }
    }

    #[must_use]
    pub const fn display_kind(&self) -> DisplayKind {
        self.accuracy.display_kind()
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Measured<U> {
        Measured {
            value: self.value.map(f),
            accuracy: self.accuracy,
        }
    }
}

impl Measured<u64> {
    /// Fold many measurements into one total.
    ///
    /// This is the function that prevents the highest-impact bug in the product:
    /// summing unavailable inputs as zero and presenting a confident, wrong
    /// dashboard total. Any missing contributor downgrades the result to
    /// `Partial`, which renders with `≥`.
    #[must_use]
    pub fn sum(parts: &[Self]) -> Self {
        let total = u32::try_from(parts.len()).unwrap_or(u32::MAX);
        if parts.is_empty() {
            return Self::unavailable(UnavailableReason::NoTelemetry {
                detail: "Nothing has been measured yet.".to_owned(),
            });
        }

        let mut acc: u64 = 0;
        let mut measured: u32 = 0;
        let mut weakest: Option<&Accuracy> = None;

        for p in parts {
            if let Some(v) = p.value {
                acc = acc.saturating_add(v);
                measured = measured.saturating_add(1);
            }
            if weakest.is_none_or(|w| p.accuracy.strength() < w.strength()) {
                weakest = Some(&p.accuracy);
            }
        }

        if measured == 0 {
            // Everything was unavailable — propagate the reason rather than
            // inventing a zero.
            let reason = parts
                .iter()
                .find_map(|p| match &p.accuracy {
                    Accuracy::Unavailable { reason } => Some(reason.clone()),
                    _ => None,
                })
                .unwrap_or(UnavailableReason::NoTelemetry {
                    detail: "No contributing measurement reported a value.".to_owned(),
                });
            return Self::unavailable(reason);
        }

        if measured < total {
            return Self::partial(acc, measured, total);
        }

        // Complete: carry the weakest certainty present.
        match weakest {
            Some(Accuracy::Exact { source }) => Self::exact(acc, *source),
            Some(Accuracy::Calculated { source }) => Self::calculated(acc, *source),
            Some(Accuracy::Estimated { .. }) => Self::estimated(acc),
            Some(Accuracy::Partial {
                measured: m,
                total: t,
            }) => Self::partial(acc, *m, *t),
            _ => Self::partial(acc, measured, total),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn exact(n: u64) -> Measured<u64> {
        Measured::exact(n, MeasurementSource::ProviderReported)
    }

    fn missing() -> Measured<u64> {
        Measured::unavailable(UnavailableReason::RequestFailed {
            detail: "The request failed before usage was reported.".to_owned(),
        })
    }

    #[test]
    fn unavailable_never_carries_a_value() {
        assert!(missing().value.is_none());
    }

    #[test]
    fn absent_provider_field_is_unavailable_not_zero() {
        // The Claude Code reasoning-token case: the field does not exist, and
        // reporting it as 0 would assert that no reasoning happened.
        let m = Measured::<u64>::from_option(
            None,
            MeasurementSource::ProviderReported,
            UnavailableReason::NotReportedByProvider {
                field: "reasoning_tokens".to_owned(),
                detail: "Claude Code does not report a reasoning-token count.".to_owned(),
            },
        );
        assert_eq!(m.display_kind(), DisplayKind::Unavailable);
        assert_ne!(m.value, Some(0));
    }

    #[test]
    fn all_exact_sums_to_exact() {
        let s = Measured::sum(&[exact(10), exact(20), exact(30)]);
        assert_eq!(s.value, Some(60));
        assert_eq!(s.display_kind(), DisplayKind::Exact);
    }

    #[test]
    fn one_missing_contributor_downgrades_the_total_to_partial() {
        // 12 exact + 1 unavailable must never render as EXACT.
        let mut parts: Vec<Measured<u64>> = (0..12).map(|_| exact(100)).collect();
        parts.push(missing());

        let s = Measured::sum(&parts);
        assert_eq!(s.value, Some(1200));
        assert_eq!(s.display_kind(), DisplayKind::Partial);
        assert!(matches!(
            s.accuracy,
            Accuracy::Partial {
                measured: 12,
                total: 13
            }
        ));
    }

    #[test]
    fn all_missing_stays_unavailable_and_never_becomes_zero() {
        let s = Measured::sum(&[missing(), missing()]);
        assert_eq!(s.value, None);
        assert_eq!(s.display_kind(), DisplayKind::Unavailable);
    }

    #[test]
    fn weakest_certainty_wins_when_complete() {
        let s = Measured::sum(&[
            exact(10),
            Measured::calculated(20, MeasurementSource::ApplicationTelemetry),
        ]);
        assert_eq!(s.value, Some(30));
        assert_eq!(s.display_kind(), DisplayKind::Calculated);
    }

    #[test]
    fn accuracy_never_recovers_by_adding_more_exact_values() {
        // Monotonicity: once degraded, no sequence of additions restores Exact.
        let degraded = Measured::sum(&[exact(1), missing()]);
        let more = Measured::sum(&[degraded, exact(5), exact(5)]);
        assert_ne!(more.display_kind(), DisplayKind::Exact);
    }

    #[test]
    fn empty_is_unavailable_not_zero() {
        assert_eq!(Measured::sum(&[]).value, None);
    }
}
