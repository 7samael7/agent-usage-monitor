//! # `aum-server` — the HTTP surface
//!
//! A library, not a binary, so integration tests can spin the whole server
//! in-process on an ephemeral port. The entire contract is therefore testable
//! without spawning a subprocess.
//!
//! The binary that wraps it is `aum-sidecar`, and it is about eighty lines.

pub mod auth;
pub mod routes;
pub mod state;

use std::net::{Ipv4Addr, SocketAddr};

use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::routing::get;
use axum::{Router, middleware};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;

pub use state::{AppState, DataHandle};

/// Bind an ephemeral loopback port.
///
/// Explicitly `127.0.0.1`, never `0.0.0.0`: binding all interfaces would expose
/// the API to the local network, and on recent macOS also triggers a
/// local-network permission prompt that has no business appearing here.
pub async fn bind() -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await
}

/// Build the router.
///
/// `/v1/health` sits outside the auth layer so the supervisor can always probe
/// liveness; everything else is behind the bearer/Host/Origin guard.
pub fn router(state: AppState) -> Router {
    let guarded = Router::new()
        .route("/v1/meta", get(routes::meta))
        .route("/v1/events", get(routes::events))
        .route("/v1/ingest/status", get(routes::ingest_status))
        .route("/v1/sessions", get(routes::sessions))
        .layer(middleware::from_fn_with_state(state.clone(), auth::guard));

    Router::new()
        .route("/v1/health", get(routes::health))
        .merge(guarded)
        // CORS sits *outside* the auth guard on purpose. A preflight `OPTIONS`
        // carries no `Authorization` header, so a guard placed outside would
        // answer it with a bare 401 and no CORS headers — and the browser would
        // report the opaque "TypeError: Failed to fetch" for every request,
        // including ones that would have authenticated perfectly well.
        .layer(cors_layer(&state))
        .with_state(state)
}

/// Exactly one allowed origin, echoed back. Never a wildcard: `*` would let any
/// page on the machine read responses if it ever learned the port and token.
fn cors_layer(state: &AppState) -> CorsLayer {
    let origin = state
        .allowed_origin()
        .parse::<HeaderValue>()
        .unwrap_or_else(|_| HeaderValue::from_static("null"));

    CorsLayer::new()
        .allow_origin(origin)
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            HeaderName::from_static("last-event-id"),
        ])
        // So a shared cache can never serve one origin's response to another.
        .vary([header::ORIGIN])
        .max_age(std::time::Duration::from_secs(600))
}

/// Serve until the process is asked to stop.
pub async fn serve(listener: TcpListener, state: AppState) -> std::io::Result<()> {
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
}

/// Wait for either interrupt or termination.
///
/// `SIGTERM` matters more than `SIGINT` here: it is what the desktop host sends
/// on quit. Listening only for Ctrl-C means every ordinary shutdown is a hard
/// kill after the host's grace period expires — survivable today, but once a
/// database is attached it means never flushing cleanly, on every single quit.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "cannot listen for SIGTERM; interrupt only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("interrupt received; shutting down"),
            _ = terminate.recv()        => tracing::info!("SIGTERM received; shutting down"),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("interrupt received; shutting down");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// Spin the real server on a real ephemeral port and return its base URL.
    async fn spawn() -> (String, AppState) {
        let listener = bind().await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = AppState::new(
            "test-token-0123456789".to_owned(),
            "app://local".to_owned(),
            port,
            "test".to_owned(),
        );
        let app = router(state.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        // Yield so the accept loop is running before the first request.
        tokio::task::yield_now().await;
        (format!("http://127.0.0.1:{port}"), state)
    }

    async fn get(url: &str, headers: &[(&str, &str)]) -> (u16, String) {
        let buf = raw_get(url, headers).await;
        let status = buf
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let body = buf.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
        (status, body)
    }

    async fn raw_get(url: &str, headers: &[(&str, &str)]) -> String {
        raw_request("GET", url, headers).await
    }

    /// A minimal HTTP/1.1 client, so the server's own test suite does not depend
    /// on a client library agreeing with us about headers. Returns the raw
    /// response, headers included, so header-level assertions are possible.
    async fn raw_request(method: &str, url: &str, headers: &[(&str, &str)]) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let addr = url.trim_start_matches("http://");
        let (host_port, path) = addr.split_once('/').map_or((addr, ""), |(h, p)| (h, p));
        let mut stream = tokio::net::TcpStream::connect(host_port).await.unwrap();

        let mut req = format!("{method} /{path} HTTP/1.1\r\nConnection: close\r\n");
        let mut saw_host = false;
        for (k, v) in headers {
            if k.eq_ignore_ascii_case("host") {
                saw_host = true;
            }
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        if !saw_host {
            req.push_str(&format!("Host: {host_port}\r\n"));
        }
        req.push_str("\r\n");

        stream.write_all(req.as_bytes()).await.unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).await.unwrap();
        buf
    }

    #[tokio::test]
    async fn health_needs_no_token() {
        let (base, _) = spawn().await;
        let (status, body) = get(&format!("{base}/v1/health"), &[]).await;
        assert_eq!(status, 200);
        assert!(body.contains("\"status\":\"ok\""));
    }

    #[tokio::test]
    async fn meta_without_a_token_is_refused() {
        let (base, _) = spawn().await;
        let (status, _) = get(&format!("{base}/v1/meta"), &[]).await;
        assert_eq!(status, 401);
    }

    #[tokio::test]
    async fn meta_with_the_token_reports_the_contract_version() {
        let (base, state) = spawn().await;
        let auth = format!("Bearer {}", state.token());
        let (status, body) = get(&format!("{base}/v1/meta"), &[("Authorization", &auth)]).await;
        assert_eq!(status, 200);
        assert!(body.contains(aum_contract::CONTRACT_VERSION));
        assert!(body.contains(&state.stream_epoch().to_string()));
    }

    #[tokio::test]
    async fn a_rebound_host_is_refused_even_with_a_valid_token() {
        let (base, state) = spawn().await;
        let auth = format!("Bearer {}", state.token());
        let (status, _) = get(
            &format!("{base}/v1/meta"),
            &[("Authorization", &auth), ("Host", "evil.example.com")],
        )
        .await;
        assert_eq!(status, 403);
    }

    /// A browser will not surface a response that lacks CORS headers, and the
    /// error it reports ("TypeError: Failed to fetch") names neither CORS nor
    /// the origin. This asserts the header is actually present.
    #[tokio::test]
    async fn responses_carry_the_allowed_origin_so_a_browser_can_read_them() {
        let (base, state) = spawn().await;
        let auth = format!("Bearer {}", state.token());
        let (status, _) = get(
            &format!("{base}/v1/meta"),
            &[("Authorization", &auth), ("Origin", "app://local")],
        )
        .await;
        assert_eq!(status, 200);

        let raw = raw_get(
            &format!("{base}/v1/meta"),
            &[("Authorization", &auth), ("Origin", "app://local")],
        )
        .await;
        assert!(
            raw.to_lowercase()
                .contains("access-control-allow-origin: app://local"),
            "missing CORS header; the browser would report only 'Failed to fetch'.\n{raw}"
        );
        assert!(raw.to_lowercase().contains("vary: origin"));
    }

    /// A preflight carries no `Authorization`. If the auth guard ran outside
    /// CORS it would answer 401 with no CORS headers, and every authenticated
    /// request from a browser would fail before it was ever sent.
    #[tokio::test]
    async fn a_preflight_without_a_token_still_gets_cors_headers() {
        let (base, _) = spawn().await;
        let raw = raw_request(
            "OPTIONS",
            &format!("{base}/v1/meta"),
            &[
                ("Origin", "app://local"),
                ("Access-Control-Request-Method", "GET"),
                ("Access-Control-Request-Headers", "authorization"),
            ],
        )
        .await;
        assert!(
            raw.to_lowercase()
                .contains("access-control-allow-origin: app://local"),
            "preflight was not answered with CORS headers:\n{raw}"
        );
    }

    #[tokio::test]
    async fn a_foreign_origin_is_refused_even_with_a_valid_token() {
        let (base, state) = spawn().await;
        let auth = format!("Bearer {}", state.token());
        let (status, _) = get(
            &format!("{base}/v1/meta"),
            &[
                ("Authorization", &auth),
                ("Origin", "https://evil.example.com"),
            ],
        )
        .await;
        assert_eq!(status, 403);
    }
}
