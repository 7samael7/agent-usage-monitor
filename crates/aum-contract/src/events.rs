//! The live event stream.
//!
//! Two rules keep the client simple and correct, and both are load-bearing:
//!
//! 1. **The stream is a hint; the database is the truth.** Every event type has a
//!    corresponding GET returning the same state, so a client that misses events
//!    is always one request away from correct.
//! 2. **Snapshots carry absolute cumulative totals, never deltas.** A dropped
//!    connection therefore cannot permanently corrupt a running total. Deltas
//!    exist only to drive animation.

use serde::{Deserialize, Serialize};

use crate::dto::{AdapterState, IngestProgress, TaskMetrics, TaskSummary};
use crate::measurement::MeasurementSource;
use crate::tokens::TokenBands;

/// One frame of the SSE stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EventEnvelope {
    /// Monotonic within a `stream_epoch`. Emitted as the SSE `id:` field so a
    /// reconnect can resume via `Last-Event-ID`.
    pub seq: i64,
    /// Changes whenever the sidecar restarts. A client seeing a new epoch must
    /// discard its live state and refetch: replaying a pre-restart store against
    /// a new backend generation is a silent-corruption bug.
    pub stream_epoch: uuid::Uuid,
    pub ts: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<uuid::Uuid>,
    #[serde(flatten)]
    pub payload: AgentEvent,
}

/// Everything the backend can push.
///
/// Internally tagged so it maps to an OpenAPI `oneOf` + `discriminator`, and
/// thence to a TypeScript discriminated union that `switch` can exhaust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Periodic liveness. Lets a client distinguish "quiet" from "half-open
    /// socket", which a health endpoint alone cannot.
    Heartbeat {
        lag_ms: u64,
    },

    TaskCreated {
        task: Box<TaskSummary>,
    },
    TaskUpdated {
        task: Box<TaskSummary>,
    },
    TaskStopped {
        exit_code: Option<i32>,
        reason: String,
    },

    /// Authoritative totals for one task. Pushed ~1 Hz per running task; this is
    /// what the UI renders.
    MetricsSnapshot {
        metrics: Box<TaskMetrics>,
    },

    /// One newly recorded request. For sparklines and the timeline; totals come
    /// from `MetricsSnapshot`, never from accumulating these.
    UsageDelta {
        request_id: uuid::Uuid,
        model_id: String,
        bands: TokenBands,
        measurement_source: MeasurementSource,
    },

    AdapterStateChanged {
        adapter_id: String,
        state: AdapterState,
    },
    IngestProgressed {
        progress: IngestProgress,
    },

    /// Usage we observed but could not attribute to any task. Surfaced rather
    /// than guessed into a task, and rather than dropped.
    AttributionUnresolved {
        session_id: String,
        adapter_id: String,
        reason: String,
    },

    /// Something did not add up. Recorded and shown; never silently corrected.
    IngestAnomaly {
        kind: String,
        detail: String,
    },

    PricingUpdated {
        model_id: String,
    },

    /// "You fell behind; refetch." Emitted when a slow client is dropped from
    /// the broadcast rather than being allowed to stall ingest.
    Resync {
        reason: String,
    },
}

impl AgentEvent {
    /// The SSE `event:` name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Heartbeat { .. } => "heartbeat",
            Self::TaskCreated { .. } => "task_created",
            Self::TaskUpdated { .. } => "task_updated",
            Self::TaskStopped { .. } => "task_stopped",
            Self::MetricsSnapshot { .. } => "metrics_snapshot",
            Self::UsageDelta { .. } => "usage_delta",
            Self::AdapterStateChanged { .. } => "adapter_state_changed",
            Self::IngestProgressed { .. } => "ingest_progressed",
            Self::AttributionUnresolved { .. } => "attribution_unresolved",
            Self::IngestAnomaly { .. } => "ingest_anomaly",
            Self::PricingUpdated { .. } => "pricing_updated",
            Self::Resync { .. } => "resync",
        }
    }
}

impl EventEnvelope {
    /// Render as an SSE frame, including the trailing blank line.
    #[must_use]
    pub fn to_sse_frame(&self) -> String {
        let data = serde_json::to_string(self).unwrap_or_else(|e| {
            format!(r#"{{"type":"ingest_anomaly","kind":"serialize","detail":"{e}"}}"#)
        });
        format!(
            "id: {}\nevent: {}\ndata: {}\n\n",
            self.seq,
            self.payload.name(),
            data
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn envelope(payload: AgentEvent) -> EventEnvelope {
        EventEnvelope {
            seq: 42,
            stream_epoch: uuid::Uuid::nil(),
            ts: chrono::Utc::now(),
            task_id: None,
            payload,
        }
    }

    #[test]
    fn sse_frame_is_well_formed() {
        let frame = envelope(AgentEvent::Heartbeat { lag_ms: 3 }).to_sse_frame();
        assert!(frame.starts_with("id: 42\nevent: heartbeat\ndata: {"));
        assert!(frame.ends_with("\n\n"));
        // The data payload must be a single line — a newline inside it would
        // split the frame and corrupt the stream.
        let data = frame.lines().nth(2).unwrap();
        assert!(data.starts_with("data: "));
    }

    #[test]
    fn event_is_internally_tagged_for_a_discriminated_union() {
        let json = serde_json::to_value(AgentEvent::Heartbeat { lag_ms: 7 }).unwrap();
        assert_eq!(json["type"], "heartbeat");
        assert_eq!(json["lag_ms"], 7);
    }

    #[test]
    fn envelope_round_trips() {
        let e = envelope(AgentEvent::Resync {
            reason: "lagged".to_owned(),
        });
        let json = serde_json::to_string(&e).unwrap();
        let back: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.seq, 42);
        assert!(matches!(back.payload, AgentEvent::Resync { .. }));
    }
}
