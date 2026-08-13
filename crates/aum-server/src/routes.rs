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

/// What ingest has read so far.
///
/// `backfilling` is what lets the UI distinguish "you have no usage" from
/// "still reading your history", which on a machine with months of transcripts
/// are very different statements.
pub async fn ingest_status(State(state): State<AppState>) -> Json<aum_contract::IngestStatus> {
    let Some(data) = state.data() else {
        return Json(aum_contract::IngestStatus::default());
    };
    let s = *data.ingest.read().await;
    Json(aum_contract::IngestStatus {
        passes: s.passes,
        files_scanned: s.cumulative.files_scanned,
        requests_recorded: s.cumulative.usage_recorded,
        anomalies: u32::try_from(s.cumulative.anomalies).unwrap_or(u32::MAX),
        backfilling: s.backfilling,
    })
}

/// Recently active sessions, whether or not a task claims them.
///
/// Unclaimed sessions are included and flagged rather than hidden: what the
/// application declined to guess about is information the user should see.
pub async fn sessions(
    State(state): State<AppState>,
) -> Result<Json<Vec<aum_contract::SessionSummary>>, (axum::http::StatusCode, String)> {
    let Some(data) = state.data() else {
        return Err((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "storage is unavailable, so no usage can be reported".to_owned(),
        ));
    };

    let rows = aum_db::repo::recent_sessions(data.db.reader(), 50)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "could not read sessions");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "could not read recorded usage".to_owned(),
            )
        })?;

    Ok(Json(rows.into_iter().map(to_summary).collect()))
}

fn to_summary(t: aum_db::repo::SessionTotals) -> aum_contract::SessionSummary {
    use aum_contract::{Measured, MeasurementSource, TokenBands, UnavailableReason};

    let n = |v: i64| u64::try_from(v).unwrap_or(0);
    let bands = TokenBands {
        input_fresh: n(t.input_fresh),
        cache_read: n(t.cache_read),
        cache_write_5m: n(t.cache_write_5m),
        cache_write_1h: n(t.cache_write_1h),
        cache_write_unspecified: n(t.cache_write_unspecified),
        output_total: n(t.output_total),
        reasoning: t.reasoning.map(n),
        unclassified: n(t.unclassified),
    };

    let requests = u32::try_from(t.requests).unwrap_or(u32::MAX);
    let reported_by = u32::try_from(t.reasoning_reported_by).unwrap_or(u32::MAX);

    // These sessions were read from files the agents wrote, so the counts are
    // the provider's own. Completeness of the *window* is a separate question
    // and belongs to a task, not to a session read from history.
    let total_tokens = Measured::exact(bands.grand_total(), MeasurementSource::ProviderReported);

    let reasoning_tokens = match t.reasoning {
        None => Measured::unavailable(UnavailableReason::NotReportedByProvider {
            field: "reasoning_tokens".to_owned(),
            detail: "This agent does not report a reasoning-token count.".to_owned(),
        }),
        Some(v) if reported_by >= requests => {
            Measured::exact(n(v), MeasurementSource::ProviderReported)
        }
        Some(v) => Measured::partial(
            n(v),
            reported_by,
            requests,
            format!("{reported_by} of {requests} requests report reasoning tokens"),
        ),
    };

    aum_contract::SessionSummary {
        session_id: t.session_id,
        adapter_id: t.adapter_id,
        model_id: t.model_id,
        requests,
        bands,
        total_tokens,
        reasoning_tokens,
        first_at: t.first_at,
        last_at: t.last_at,
        unattributed: t.unattributed,
    }
}
