//! # `aum-contract` — the vocabulary
//!
//! Every type in this crate exists to make a number carry how well it is known.
//! It has **no internal dependencies**, and did not acquire any when everything
//! around it changed.
//!
//! It began as a wire contract: this was the boundary between an Electron app
//! and a Rust backend over loopback HTTP, and the promise was that a Go or
//! Python reimplementation serving the same OpenAPI document would need no
//! frontend changes. The HTTP layer is gone and the interface is a terminal in
//! the same process, so there is no wire and nothing to be compatible with.
//!
//! What survived the deletion is the part that was never about transport. These
//! are the words the whole codebase uses to talk about certainty, and they turned
//! out to be domain vocabulary that happened to serialise well rather than wire
//! types that happened to be useful.
//!
//! ## Conventions that carry meaning
//!
//! * Every enum here is **internally tagged**, so a serialised
//!   [`measurement::Accuracy`] names itself. It was chosen for OpenAPI
//!   `discriminator` and TypeScript unions; it is kept because `--json` output
//!   that says `"kind": "partial"` beside a value is self-describing to whatever
//!   reads it next.
//! * [`money::Money`] serialises as a **decimal string**, and its deserializer
//!   *rejects* a JSON number rather than accepting a float's rounding.
//! * [`measurement::Measured`] is the only way a number reaches a screen, so a
//!   missing value cannot be rendered as zero.

pub mod dto;
pub mod measurement;
pub mod money;
pub mod tokens;

pub use dto::{
    AdapterDescriptor, AdapterState, CapabilityState, Comparison, ComparisonRow, CostBreakdown,
    DailyPoint, DailyTotal, FxRow, HealthResponse, IngestProgress, IngestStatus, LatencySummary,
    MetaResponse, NewFxRate, NewPrice, Normalized, ObservedModel, PriceRow, PricingView,
    RequestCounts, SeriesPoint, SessionSummary, TaskBinding, TaskMetrics, TaskStatus, TaskSummary,
};
pub use measurement::{Accuracy, DisplayKind, Measured, MeasurementSource, UnavailableReason};
pub use money::{Currency, Money};
pub use tokens::TokenBands;
