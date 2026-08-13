//! Shared server state.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Instant;

use aum_contract::{AgentEvent, EventEnvelope};
use tokio::sync::{RwLock, broadcast};

/// What the server needs in order to answer questions about recorded usage.
///
/// Optional so the transport layer can be tested without storage, and so the
/// process still serves `/v1/health` if the database could not be opened —
/// failing to open it should produce a diagnosable app, not a silent one.
#[derive(Clone)]
pub struct DataHandle {
    pub db: aum_db::Database,
    pub ingest: std::sync::Arc<RwLock<aum_engine::IngestState>>,
}

/// How many events the broadcast buffer holds before a slow subscriber is
/// dropped.
///
/// This channel is **lossy on purpose**. A browser that stops reading must never
/// be able to stall ingest — dropping tokens to keep a UI happy would be exactly
/// backwards. A dropped subscriber receives [`AgentEvent::Resync`] and refetches,
/// which is always correct because the database, not the stream, is the truth.
const BUS_CAPACITY: usize = 1024;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

struct Inner {
    started_at: Instant,
    token: String,
    allowed_origin: String,
    port: u16,
    /// Regenerated on every process start. A client that sees a new epoch
    /// discards its live state rather than replaying a previous generation's
    /// numbers into a new one.
    stream_epoch: uuid::Uuid,
    bus: broadcast::Sender<EventEnvelope>,
    seq: AtomicI64,
    impl_version: String,
    data: Option<DataHandle>,
}

impl AppState {
    #[must_use]
    pub fn new(token: String, allowed_origin: String, port: u16, impl_version: String) -> Self {
        Self::with_data(token, allowed_origin, port, impl_version, None)
    }

    #[must_use]
    pub fn with_data(
        token: String,
        allowed_origin: String,
        port: u16,
        impl_version: String,
        data: Option<DataHandle>,
    ) -> Self {
        let (bus, _) = broadcast::channel(BUS_CAPACITY);
        Self(Arc::new(Inner {
            started_at: Instant::now(),
            token,
            allowed_origin,
            port,
            stream_epoch: uuid::Uuid::new_v4(),
            bus,
            seq: AtomicI64::new(0),
            impl_version,
            data,
        }))
    }

    /// `None` when storage is unavailable, so handlers can say so explicitly
    /// rather than returning an empty list that looks like "no usage yet".
    #[must_use]
    pub fn data(&self) -> Option<&DataHandle> {
        self.0.data.as_ref()
    }

    #[cfg(test)]
    #[must_use]
    pub fn for_test(token: &str, origin: &str, port: u16) -> Self {
        Self::new(token.to_owned(), origin.to_owned(), port, "test".to_owned())
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.0.token
    }

    #[must_use]
    pub fn stream_epoch(&self) -> uuid::Uuid {
        self.0.stream_epoch
    }

    #[must_use]
    pub fn impl_version(&self) -> &str {
        &self.0.impl_version
    }

    #[must_use]
    pub fn uptime_ms(&self) -> u64 {
        u64::try_from(self.0.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Only the loopback address on the port we actually bound.
    #[must_use]
    pub fn is_allowed_host(&self, host: &str) -> bool {
        let expected_v4 = format!("127.0.0.1:{}", self.0.port);
        let expected_name = format!("localhost:{}", self.0.port);
        let expected_v6 = format!("[::1]:{}", self.0.port);
        host == expected_v4 || host == expected_name || host == expected_v6
    }

    #[must_use]
    pub fn is_allowed_origin(&self, origin: &str) -> bool {
        origin == self.0.allowed_origin
    }

    #[must_use]
    pub fn allowed_origin(&self) -> &str {
        &self.0.allowed_origin
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.0.bus.subscribe()
    }

    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.0.bus.receiver_count()
    }

    /// Publish an event. Returns the sequence number assigned.
    ///
    /// A send failure means nobody is listening, which is normal and not an
    /// error — the event is already durable elsewhere.
    pub fn publish(&self, task_id: Option<uuid::Uuid>, payload: AgentEvent) -> i64 {
        let seq = self.0.seq.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        let envelope = EventEnvelope {
            seq,
            stream_epoch: self.0.stream_epoch,
            ts: chrono::Utc::now(),
            task_id,
            payload,
        };
        let _ = self.0.bus.send(envelope);
        seq
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn sequence_numbers_are_monotonic() {
        let s = AppState::for_test("t", "app://local", 1234);
        let a = s.publish(None, AgentEvent::Heartbeat { lag_ms: 0 });
        let b = s.publish(None, AgentEvent::Heartbeat { lag_ms: 0 });
        assert!(b > a);
    }

    #[test]
    fn publishing_with_no_subscribers_is_not_an_error() {
        let s = AppState::for_test("t", "app://local", 1234);
        assert_eq!(s.subscriber_count(), 0);
        s.publish(None, AgentEvent::Heartbeat { lag_ms: 0 });
    }

    #[test]
    fn each_process_start_gets_a_fresh_epoch() {
        let a = AppState::for_test("t", "app://local", 1234);
        let b = AppState::for_test("t", "app://local", 1234);
        assert_ne!(a.stream_epoch(), b.stream_epoch());
    }

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let s = AppState::for_test("t", "app://local", 1234);
        let mut rx = s.subscribe();
        s.publish(None, AgentEvent::Heartbeat { lag_ms: 42 });
        let got = rx.recv().await.unwrap();
        assert!(matches!(got.payload, AgentEvent::Heartbeat { lag_ms: 42 }));
        assert_eq!(got.stream_epoch, s.stream_epoch());
    }

    #[tokio::test]
    async fn a_slow_subscriber_lags_rather_than_blocking_the_publisher() {
        // The critical property: ingest must never be stalled by a stuck UI.
        let s = AppState::for_test("t", "app://local", 1234);
        let mut rx = s.subscribe();
        for _ in 0..(BUS_CAPACITY + 10) {
            s.publish(None, AgentEvent::Heartbeat { lag_ms: 0 });
        }
        match rx.recv().await {
            Err(broadcast::error::RecvError::Lagged(n)) => assert!(n > 0),
            other => panic!("expected the slow subscriber to be lagged, got {other:?}"),
        }
    }
}
