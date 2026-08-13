//! # `aum-contract` — the wire contract
//!
//! This crate is the boundary between the desktop app and whatever implements
//! the backend. It has **no internal dependencies**, and that is the point: a
//! Go, C#, or Python reimplementation satisfies the same OpenAPI document and
//! the desktop app needs no changes at all.
//!
//! What a replacement backend must provide, in full:
//!
//! 1. Bind `127.0.0.1:0` and print one [`handshake::Handshake`] line to stdout;
//!    log to stderr.
//! 2. Serve the paths in `packages/api-contract/openapi.json` with matching
//!    schemas.
//! 3. Serve `GET /v1/events` as SSE with `id:` lines, honouring `Last-Event-ID`.
//! 4. Enforce bearer token + `Host` + `Origin`.
//!
//! Nothing else. No shared library, no FFI, no IPC framing.
//!
//! ## Conventions that carry meaning
//!
//! * Every enum here is **internally tagged**. Internally-tagged enums map
//!   cleanly to OpenAPI `oneOf` + `discriminator` *and* to TypeScript
//!   discriminated unions; externally-tagged ones do not.
//! * [`money::Money`] serializes as a **decimal string**, never a JSON number.
//! * [`measurement::Measured`] is the only way a number reaches the UI, so a
//!   missing value cannot be rendered as zero.

pub mod dto;
pub mod events;
pub mod handshake;
pub mod measurement;
pub mod money;
pub mod tokens;

/// Semver of the HTTP contract. The desktop app refuses to run against a
/// backend whose major version differs.
pub const CONTRACT_VERSION: &str = "1.0.0";

/// Path prefix for every versioned route.
pub const API_PREFIX: &str = "/v1";

pub use dto::{
    AdapterDescriptor, AdapterState, CapabilityState, CostBreakdown, HealthResponse,
    IngestProgress, LatencySummary, MetaResponse, RequestCounts, TaskBinding, TaskMetrics,
    TaskStatus, TaskSummary,
};
pub use events::{AgentEvent, EventEnvelope};
pub use handshake::Handshake;
pub use measurement::{Accuracy, DisplayKind, Measured, MeasurementSource, UnavailableReason};
pub use money::{Currency, Money};
pub use tokens::TokenBands;
