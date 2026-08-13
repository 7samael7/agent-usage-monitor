//! HTTP handlers.

use axum::Json;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use std::convert::Infallible;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use aum_contract::{AgentEvent, HealthResponse, MetaResponse};

use crate::state::AppState;

/// Liveness. Deliberately unauthenticated so the host supervisor can probe a
/// backend whose token it may have lost track of after a restart. It exposes
/// nothing beyond "this process is up".
pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_owned(),
        uptime_ms: state.uptime_ms(),
    })
}

/// What this backend is and what it can do.
///
/// `capabilities` lists what is genuinely wired up, so the UI can hide controls
/// that would do nothing rather than offering them and failing.
pub async fn meta(State(state): State<AppState>) -> Json<MetaResponse> {
    Json(MetaResponse {
        contract_version: aum_contract::CONTRACT_VERSION.to_owned(),
        impl_name: "aum-sidecar-rust".to_owned(),
        impl_version: state.impl_version().to_owned(),
        stream_epoch: state.stream_epoch(),
        capabilities: vec!["events".to_owned()],
    })
}

/// The live event stream.
///
/// A subscriber that falls behind is dropped from the broadcast rather than
/// being allowed to apply backpressure to ingest. It is told so explicitly via
/// [`AgentEvent::Resync`], and refetching is always correct because the database,
/// not this stream, is the source of truth.
pub async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let epoch = state.stream_epoch();

    let stream = BroadcastStream::new(state.subscribe()).map(move |item| {
        let event = match item {
            Ok(envelope) => Event::default()
                .id(envelope.seq.to_string())
                .event(envelope.payload.name())
                .json_data(&envelope)
                .unwrap_or_else(|e| Event::default().event("ingest_anomaly").data(e.to_string())),

            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "SSE subscriber lagged; asking it to resync");
                let payload = AgentEvent::Resync {
                    reason: format!("{skipped} events were skipped; refetch current state"),
                };
                Event::default()
                    .event(payload.name())
                    .json_data(aum_contract::EventEnvelope {
                        seq: -1,
                        stream_epoch: epoch,
                        ts: chrono::Utc::now(),
                        task_id: None,
                        payload,
                    })
                    .unwrap_or_else(|_| Event::default().event("resync").data("{}"))
            }
        };
        Ok(event)
    });

    // A comment-only keep-alive every 15s, so a client can tell a quiet stream
    // from a half-open socket — something a health endpoint cannot distinguish.
    Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
}
