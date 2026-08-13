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
        files_skipped: s.cumulative.files_skipped,
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

// ── Tasks ───────────────────────────────────────────────────────────────────

fn no_storage() -> (axum::http::StatusCode, String) {
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "storage is unavailable, so tasks cannot be managed".to_owned(),
    )
}

fn server_error(e: impl std::fmt::Display, what: &str) -> (axum::http::StatusCode, String) {
    tracing::error!(error = %e, "{what}");
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        what.to_owned(),
    )
}

pub async fn list_tasks(
    State(state): State<AppState>,
) -> Result<Json<Vec<aum_contract::TaskSummary>>, (axum::http::StatusCode, String)> {
    let data = state.data().ok_or_else(no_storage)?;
    let rows = aum_db::repo::list_tasks(data.db.reader(), 200)
        .await
        .map_err(|e| server_error(e, "could not list tasks"))?;
    Ok(Json(rows.into_iter().map(to_task_summary).collect()))
}

pub async fn task_metrics(
    State(state): State<AppState>,
    axum::extract::Path(task_id): axum::extract::Path<uuid::Uuid>,
    axum::extract::Query(q): axum::extract::Query<CurrencyQuery>,
) -> Result<Json<aum_contract::TaskMetrics>, (axum::http::StatusCode, String)> {
    let data = state.data().ok_or_else(no_storage)?;
    let money = data.money.read().await;
    let metrics = aum_engine::task_metrics(&data.db, task_id, money.cost_context(q.or_usd()))
        .await
        .map_err(|e| server_error(e, "could not compute task metrics"))?;
    Ok(Json(metrics))
}

#[derive(serde::Deserialize)]
pub struct CreateTask {
    pub name: String,
    pub adapter_id: String,
    pub working_dir: String,
    pub prompt: String,
    #[serde(default)]
    pub benchmark_id: Option<uuid::Uuid>,
    /// Environment for the child process.
    ///
    /// Accepted, passed to the agent, and never stored or echoed back: these
    /// routinely hold API keys.
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

pub async fn create_task(
    State(state): State<AppState>,
    Json(body): Json<CreateTask>,
) -> Result<Json<aum_contract::TaskSummary>, (axum::http::StatusCode, String)> {
    let data = state.data().ok_or_else(no_storage)?;

    let agent = aum_engine::Agent::parse(&body.adapter_id).ok_or_else(|| {
        (
            axum::http::StatusCode::BAD_REQUEST,
            format!("{} cannot be launched by the monitor", body.adapter_id),
        )
    })?;

    let spec = aum_engine::TaskSpec {
        name: body.name,
        agent,
        working_dir: std::path::PathBuf::from(body.working_dir),
        prompt: body.prompt,
        benchmark_id: body.benchmark_id,
        env: body.env,
    };

    let task_id = data.tasks.launch(&spec).await.map_err(|e| {
        // A missing agent is the user's problem to fix and the message says how,
        // so it is a 400 rather than a 500.
        let status = match e {
            aum_engine::TaskError::AgentMissing { .. } => axum::http::StatusCode::BAD_REQUEST,
            _ => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, e.to_string())
    })?;

    let rows = aum_db::repo::list_tasks(data.db.reader(), 200)
        .await
        .map_err(|e| server_error(e, "could not read the task back"))?;
    let row = rows
        .into_iter()
        .find(|r| r.id == task_id.to_string())
        .ok_or_else(|| server_error("missing", "the task vanished after being created"))?;

    let summary = to_task_summary(row);
    state.publish(
        Some(task_id),
        aum_contract::AgentEvent::TaskCreated {
            task: Box::new(summary.clone()),
        },
    );
    Ok(Json(summary))
}

pub async fn stop_task(
    State(state): State<AppState>,
    axum::extract::Path(task_id): axum::extract::Path<uuid::Uuid>,
) -> Result<axum::http::StatusCode, (axum::http::StatusCode, String)> {
    let data = state.data().ok_or_else(no_storage)?;
    data.tasks
        .stop(task_id)
        .await
        .map_err(|e| server_error(e, "could not stop the task"))?;

    state.publish(
        Some(task_id),
        aum_contract::AgentEvent::TaskStopped {
            exit_code: None,
            reason: "stopped by the user".to_owned(),
        },
    );
    Ok(axum::http::StatusCode::NO_CONTENT)
}

fn to_task_summary(row: aum_db::repo::TaskRow) -> aum_contract::TaskSummary {
    use aum_contract::{TaskBinding, TaskStatus};

    let binding = match (row.binding_method.as_deref(), row.session_id) {
        (Some("launched_pinned"), Some(session_id)) => TaskBinding::LaunchedPinned { session_id },
        (Some("launched_stdout"), Some(session_id)) => TaskBinding::SessionIdExact { session_id },
        (Some("session_id_exact"), Some(session_id)) => TaskBinding::SessionIdExact { session_id },
        // No binding yet. A launched Codex task is briefly in this state, until
        // its stream announces a session id — and it is shown as unbound rather
        // than as something we have guessed.
        _ => TaskBinding::Unbound,
    };

    aum_contract::TaskSummary {
        id: uuid::Uuid::parse_str(&row.id).unwrap_or_default(),
        benchmark_id: row
            .benchmark_id
            .and_then(|b| uuid::Uuid::parse_str(&b).ok()),
        name: row.name,
        adapter_id: row.adapter_id,
        status: match row.status.as_str() {
            "running" => TaskStatus::Running,
            "completed" => TaskStatus::Completed,
            "failed" => TaskStatus::Failed,
            "stopped" => TaskStatus::Stopped,
            _ => TaskStatus::Pending,
        },
        binding,
        working_dir: row.working_dir,
        model_id: row.model_id,
        started_at: row
            .started_at
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
            .map(|t| t.with_timezone(&chrono::Utc)),
        ended_at: row
            .ended_at
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok())
            .map(|t| t.with_timezone(&chrono::Utc)),
    }
}

// ── Applications ────────────────────────────────────────────────────────────

/// What each monitored application can actually tell us.
///
/// Derived by running the real parsers over the applications' own recent files,
/// so the matrix reports what was observed rather than what was hoped.
pub async fn adapters(
    State(_state): State<AppState>,
) -> Json<Vec<aum_contract::AdapterDescriptor>> {
    use aum_adapters::{claude_code::ClaudeCodeAdapter, codex::CodexAdapter};

    let home = dirs::home_dir().unwrap_or_default();

    fn is_jsonl(p: &std::path::Path) -> bool {
        p.extension().is_some_and(|e| e == "jsonl")
    }
    fn is_rollout(p: &std::path::Path) -> bool {
        is_jsonl(p)
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-"))
    }

    // Probing reads files, so it runs on the blocking pool rather than stalling
    // the async runtime.
    let descriptors = tokio::task::spawn_blocking(move || {
        let claude_root = home.join(".claude").join("projects");
        let codex_root = home.join(".codex").join("sessions");

        let claude = aum_engine::describe_file_adapter(
            "claude_code",
            "Claude Code",
            &aum_engine::probe(&ClaudeCodeAdapter, &claude_root, &is_jsonl),
            aum_procmon::launch::discover("claude").map(|p| p.display().to_string()),
            claude_root.is_dir(),
        );

        let codex = aum_engine::describe_file_adapter(
            "codex",
            "Codex",
            &aum_engine::probe(&CodexAdapter, &codex_root, &is_rollout),
            aum_procmon::launch::discover("codex").map(|p| p.display().to_string()),
            codex_root.is_dir(),
        );

        let desktop = aum_engine::describe_claude_desktop(
            std::path::Path::new("/Applications/Claude.app").is_dir(),
        );

        vec![claude, codex, desktop]
    })
    .await
    .unwrap_or_default();

    Json(descriptors)
}

// ── Series and export ───────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct SeriesQuery {
    /// Bucket width. Defaults to a minute, which gives a readable line for a
    /// task of any realistic length.
    #[serde(default)]
    pub bucket_seconds: Option<i64>,
}

/// Usage over time, bucketed in the database.
pub async fn task_series(
    State(state): State<AppState>,
    axum::extract::Path(task_id): axum::extract::Path<uuid::Uuid>,
    axum::extract::Query(query): axum::extract::Query<SeriesQuery>,
) -> Result<Json<Vec<aum_contract::SeriesPoint>>, (axum::http::StatusCode, String)> {
    let data = state.data().ok_or_else(no_storage)?;
    let buckets = aum_db::repo::task_series(
        data.db.reader(),
        &task_id.to_string(),
        query.bucket_seconds.unwrap_or(60),
    )
    .await
    .map_err(|e| server_error(e, "could not read the series"))?;

    Ok(Json(
        buckets
            .into_iter()
            .map(|b| aum_contract::SeriesPoint {
                at: b.at,
                requests: u32::try_from(b.requests).unwrap_or(0),
                input_fresh: u64::try_from(b.input_fresh).unwrap_or(0),
                cache_read: u64::try_from(b.cache_read).unwrap_or(0),
                cache_write: u64::try_from(b.cache_write).unwrap_or(0),
                output_total: u64::try_from(b.output_total).unwrap_or(0),
                unclassified: u64::try_from(b.unclassified).unwrap_or(0),
            })
            .collect(),
    ))
}

#[derive(serde::Deserialize)]
pub struct ExportQuery {
    /// `json` or `csv`.
    #[serde(default)]
    pub format: Option<String>,
    /// Presentation currency, so an exported cost matches what was on screen.
    #[serde(default)]
    pub currency: Option<CurrencyCode>,
}

/// Export a task's requests.
///
/// Metadata only. Content capture is off by default so there is none to export,
/// and an export must not become the one path by which conversation data leaves
/// the machine.
pub async fn export_task(
    State(state): State<AppState>,
    axum::extract::Path(task_id): axum::extract::Path<uuid::Uuid>,
    axum::extract::Query(query): axum::extract::Query<ExportQuery>,
) -> Result<axum::response::Response, (axum::http::StatusCode, String)> {
    use axum::response::IntoResponse as _;

    let data = state.data().ok_or_else(no_storage)?;
    let id = task_id.to_string();

    let rows = aum_db::repo::task_requests(data.db.reader(), &id)
        .await
        .map_err(|e| server_error(e, "could not read the task's requests"))?;

    if query.format.as_deref() == Some("csv") {
        let mut out = String::from(
            "occurred_at,adapter,session_id,model,measurement_source,request_kind,\
             input_fresh,cache_read,cache_write_5m,cache_write_1h,cache_write_unspecified,\
             output_total,reasoning,unclassified,is_sidechain,agent_type\n",
        );
        for r in &rows {
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                r.occurred_at,
                r.adapter_id,
                r.session_id,
                r.model_id.as_deref().unwrap_or(""),
                r.measurement_source,
                r.request_kind,
                r.input_fresh,
                r.cache_read,
                r.cache_write_5m,
                r.cache_write_1h,
                r.cache_write_unspecified,
                r.output_total,
                // An empty cell, not a zero: the provider reported nothing, and
                // a spreadsheet summing a column of zeros would be wrong.
                r.reasoning.map(|v| v.to_string()).unwrap_or_default(),
                r.unclassified,
                r.is_sidechain,
                r.agent_type.as_deref().unwrap_or(""),
            ));
        }
        return Ok((
            [
                (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
                (
                    axum::http::header::CONTENT_DISPOSITION,
                    "attachment; filename=\"task-export.csv\"",
                ),
            ],
            out,
        )
            .into_response());
    }

    let money = data.money.read().await;
    let metrics = aum_engine::task_metrics(
        &data.db,
        task_id,
        money.cost_context(query.currency.map_or(aum_contract::Currency::Usd, |c| c.0)),
    )
    .await
    .map_err(|e| server_error(e, "could not compute task metrics"))?;

    Ok(Json(serde_json::json!({
        "exported_at": chrono::Utc::now(),
        "task_id": id,
        "metrics": metrics,
        "requests": rows,
        "note": "Metadata only. Prompt and response text are not recorded by this application.",
    }))
    .into_response())
}

// ── Pricing ─────────────────────────────────────────────────────────────────

/// Which currency to present amounts in, from `?currency=EUR`.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
pub struct CurrencyQuery {
    #[serde(default)]
    pub currency: Option<CurrencyCode>,
}

/// A currency code that only deserializes if this backend can actually present
/// it — an unsupported one is a 400, not a silent fallback to dollars under a
/// euro label.
#[derive(Debug, Clone, Copy)]
pub struct CurrencyCode(pub aum_contract::Currency);

impl<'de> serde::Deserialize<'de> for CurrencyCode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        aum_engine::prices::parse_currency(&raw)
            .map(Self)
            .ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "{raw} is not a currency this backend can present"
                ))
            })
    }
}

impl CurrencyQuery {
    fn or_usd(self) -> aum_contract::Currency {
        self.currency.map_or(aum_contract::Currency::Usd, |c| c.0)
    }
}

/// Everything the pricing screen needs: what ran, what it costs, and what a
/// converted amount would be converted with.
pub async fn pricing(
    State(state): State<AppState>,
) -> Result<Json<aum_contract::PricingView>, (axum::http::StatusCode, String)> {
    let data = state.data().ok_or_else(no_storage)?;

    let observed = aum_db::repo::observed_models(data.db.reader())
        .await
        .map_err(|e| server_error(e, "could not read the models in use"))?;

    let money = data.money.read().await;
    let now = chrono::Utc::now();

    let models = observed
        .into_iter()
        .map(|m| aum_contract::ObservedModel {
            priced: money.table.has_price(&m.model_id),
            model_id: m.model_id,
            adapter_id: m.adapter_id,
            requests: u32::try_from(m.requests).unwrap_or(u32::MAX),
            total_tokens: u64::try_from(m.total_tokens).unwrap_or(0),
        })
        .collect();

    let prices = money
        .table
        .models()
        .into_iter()
        .map(|p| {
            let money_of = |d: rust_decimal::Decimal| aum_contract::Money::new(d);
            aum_contract::PriceRow {
                // The version that would be used right now, which is not
                // necessarily the newest row: a user entry outranks a seeded one.
                is_current: money
                    .table
                    .lookup(&p.model_id)
                    .is_some_and(|cur| cur.version_id == p.version_id),
                version_id: p.version_id.clone(),
                model_id: p.model_id.clone(),
                input_per_mtok: money_of(p.rates.input_per_mtok),
                output_per_mtok: money_of(p.rates.output_per_mtok),
                cache_read_per_mtok: money_of(p.rates.cache_read_per_mtok),
                cache_write_5m_per_mtok: money_of(p.rates.cache_write_5m_per_mtok),
                cache_write_1h_per_mtok: money_of(p.rates.cache_write_1h_per_mtok),
                effective_from: p.effective_from.clone(),
                source: p.source.clone(),
            }
        })
        .collect();

    let fx = money
        .fx
        .iter()
        .map(|r| aum_contract::FxRow {
            quote_currency: r.quote.code().to_owned(),
            rate: aum_contract::Money::new(r.rate),
            as_of: r.as_of.to_rfc3339(),
            source: r.source.clone(),
            age_days: r.age_days(now),
            is_stale: r.is_stale(now),
            description: aum_pricing::fx::describe(Some(r), now),
        })
        .collect();

    Ok(Json(aum_contract::PricingView {
        models,
        prices,
        fx,
        supported_currencies: vec!["USD".to_owned(), "EUR".to_owned(), "CZK".to_owned()],
    }))
}

/// Record a price the user has entered.
///
/// The rates go in as a new version, and the in-memory table is rebuilt from
/// storage rather than patched, so what the next request costs with is exactly
/// what was persisted.
pub async fn set_price(
    State(state): State<AppState>,
    Json(body): Json<aum_contract::NewPrice>,
) -> Result<Json<aum_contract::PriceRow>, (axum::http::StatusCode, String)> {
    let data = state.data().ok_or_else(no_storage)?;

    if body.model_id.trim().is_empty() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "a price needs the model id it applies to".to_owned(),
        ));
    }

    let input = body.input_per_mtok.amount();
    // Absent means "charged at the input rate", which is both providers'
    // documented default. Zero would be a claim that caching is free, and would
    // understate a long cached session by most of its total.
    let rates = aum_pricing::Rates {
        input_per_mtok: input,
        output_per_mtok: body.output_per_mtok.amount(),
        cache_read_per_mtok: body.cache_read_per_mtok.map_or(input, |m| m.amount()),
        cache_write_5m_per_mtok: body.cache_write_5m_per_mtok.map_or(input, |m| m.amount()),
        cache_write_1h_per_mtok: body.cache_write_1h_per_mtok.map_or(input, |m| m.amount()),
    };

    if rates.input_per_mtok.is_sign_negative() || rates.output_per_mtok.is_sign_negative() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "a rate cannot be negative".to_owned(),
        ));
    }

    let saved = aum_engine::prices::save_price(&data.db, &body.model_id, &rates, body.note)
        .await
        .map_err(|e| match e {
            aum_engine::prices::PriceError::Unstorable { .. } => {
                (axum::http::StatusCode::BAD_REQUEST, e.to_string())
            }
            aum_engine::prices::PriceError::Db(inner) => {
                server_error(inner, "could not record the price")
            }
        })?;

    reload_money(&state).await?;

    Ok(Json(aum_contract::PriceRow {
        version_id: saved.version_id,
        model_id: saved.model_id,
        input_per_mtok: aum_contract::Money::new(saved.rates.input_per_mtok),
        output_per_mtok: aum_contract::Money::new(saved.rates.output_per_mtok),
        cache_read_per_mtok: aum_contract::Money::new(saved.rates.cache_read_per_mtok),
        cache_write_5m_per_mtok: aum_contract::Money::new(saved.rates.cache_write_5m_per_mtok),
        cache_write_1h_per_mtok: aum_contract::Money::new(saved.rates.cache_write_1h_per_mtok),
        effective_from: saved.effective_from,
        source: saved.source,
        is_current: true,
    }))
}

/// Record an exchange rate the user has entered.
///
/// Typed in, never fetched: this process makes no outbound request to find one,
/// which is what keeps the privacy claim in Settings true.
pub async fn set_fx_rate(
    State(state): State<AppState>,
    Json(body): Json<aum_contract::NewFxRate>,
) -> Result<Json<aum_contract::FxRow>, (axum::http::StatusCode, String)> {
    let data = state.data().ok_or_else(no_storage)?;

    let currency = aum_engine::prices::parse_currency(&body.quote_currency).ok_or((
        axum::http::StatusCode::BAD_REQUEST,
        format!(
            "{} is not a currency this backend can present",
            body.quote_currency
        ),
    ))?;

    if currency == aum_contract::Currency::Usd {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "USD is the base currency and needs no rate".to_owned(),
        ));
    }

    if body.rate.amount() <= rust_decimal::Decimal::ZERO {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            "an exchange rate must be greater than zero".to_owned(),
        ));
    }

    let saved = aum_engine::prices::save_fx(&data.db, currency, body.rate.amount())
        .await
        .map_err(|e| match e {
            aum_engine::prices::PriceError::Unstorable { .. } => {
                (axum::http::StatusCode::BAD_REQUEST, e.to_string())
            }
            aum_engine::prices::PriceError::Db(inner) => {
                server_error(inner, "could not record the exchange rate")
            }
        })?;

    reload_money(&state).await?;

    let now = chrono::Utc::now();
    Ok(Json(aum_contract::FxRow {
        quote_currency: saved.quote.code().to_owned(),
        rate: aum_contract::Money::new(saved.rate),
        as_of: saved.as_of.to_rfc3339(),
        source: saved.source.clone(),
        age_days: saved.age_days(now),
        is_stale: saved.is_stale(now),
        description: aum_pricing::fx::describe(Some(&saved), now),
    }))
}

/// Rebuild the in-memory rates from storage.
async fn reload_money(state: &AppState) -> Result<(), (axum::http::StatusCode, String)> {
    let data = state.data().ok_or_else(no_storage)?;
    let table = aum_engine::prices::load_table(&data.db)
        .await
        .map_err(|e| server_error(e, "could not reload prices"))?;
    let fx = aum_engine::prices::load_fx(&data.db)
        .await
        .map_err(|e| server_error(e, "could not reload exchange rates"))?;

    let mut money = data.money.write().await;
    *money = crate::state::MoneyState { table, fx };
    Ok(())
}

// ── Comparison ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct CompareQuery {
    /// Comma-separated task ids, in the order to show them.
    #[serde(default)]
    pub tasks: Option<String>,
    #[serde(default)]
    pub currency: Option<CurrencyCode>,
    /// Divide each task's figures by the output it produced.
    #[serde(default)]
    pub normalize: Option<bool>,
}

/// How many tasks one comparison may hold.
///
/// Each row is a separate aggregate query, and a table nobody can read is not
/// worth the round trips. Asking for more is an error rather than a silent
/// truncation — quietly dropping rows from a comparison would make the answer
/// wrong in a way the screen could not show.
const MAX_COMPARED: usize = 25;

pub async fn compare(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<CompareQuery>,
) -> Result<Json<aum_contract::Comparison>, (axum::http::StatusCode, String)> {
    let data = state.data().ok_or_else(no_storage)?;

    let raw = query.tasks.unwrap_or_default();
    let mut ids = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let id = uuid::Uuid::parse_str(part).map_err(|_| {
            (
                axum::http::StatusCode::BAD_REQUEST,
                format!("{part} is not a task id"),
            )
        })?;
        // A task named twice would appear twice and be compared with itself.
        if !ids.contains(&id) {
            ids.push(id);
        }
    }

    if ids.len() > MAX_COMPARED {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            format!(
                "a comparison holds at most {MAX_COMPARED} tasks; {} were asked for",
                ids.len()
            ),
        ));
    }

    let money = data.money.read().await;
    let ctx = money.cost_context(query.currency.map_or(aum_contract::Currency::Usd, |c| c.0));

    let comparison =
        aum_engine::compare::compare(&data.db, &ids, ctx, query.normalize == Some(true))
            .await
            .map_err(|e| server_error(e, "could not build the comparison"))?;

    Ok(Json(comparison))
}
