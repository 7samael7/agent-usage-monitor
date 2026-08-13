//! The sidecar → host handshake.
//!
//! The sidecar writes **exactly one line** of JSON to stdout and then never uses
//! stdout again; all logging goes to stderr as NDJSON. This is the entire
//! bootstrap protocol, and it is deliberately the only thing the desktop host
//! knows about the backend besides the HTTP contract.
//!
//! The bearer token travels in the environment, **never in argv** — argv is
//! world-readable via `ps aux` — and is never written to disk, so there is no
//! stale-file race after a crash and no file-permission question.

use serde::{Deserialize, Serialize};

/// Wire protocol version. Bumped only for breaking changes to the handshake
/// itself, which is expected to be approximately never.
pub const HANDSHAKE_PROTOCOL: u32 = 1;

/// Environment variable carrying the bearer token the host generated.
pub const ENV_TOKEN: &str = "AUM_TOKEN";
/// Environment variable carrying the host's PID, for the orphan watchdog.
pub const ENV_PARENT_PID: &str = "AUM_PARENT_PID";
/// Environment variable carrying the data directory (SQLite lives here).
pub const ENV_DATA_DIR: &str = "AUM_DATA_DIR";
/// Environment variable carrying the exact origin allowed to call us.
pub const ENV_ALLOWED_ORIGIN: &str = "AUM_ALLOWED_ORIGIN";

/// The single line the sidecar prints to stdout once it is listening.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Handshake {
    /// Always `"aum.handshake"`. Lets the host reject unrelated stdout noise
    /// instead of misparsing it.
    pub kind: String,
    pub protocol: u32,
    /// The ephemeral loopback port the server bound.
    pub port: u16,
    pub pid: u32,
    /// Semver of the HTTP contract. The host refuses to run against a mismatch
    /// rather than degrading gracefully — degrading gracefully across a contract
    /// break is precisely how a UI starts rendering confident wrong numbers.
    pub contract_version: String,
    /// Identifies the backend implementation. A Go or C# reimplementation puts
    /// its own name here; nothing in the desktop app branches on it.
    pub impl_name: String,
    pub impl_version: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

impl Handshake {
    pub const KIND: &'static str = "aum.handshake";

    #[must_use]
    pub fn new(port: u16, pid: u32, contract_version: &str, impl_version: &str) -> Self {
        Self {
            kind: Self::KIND.to_owned(),
            protocol: HANDSHAKE_PROTOCOL,
            port,
            pid,
            contract_version: contract_version.to_owned(),
            impl_name: "aum-sidecar-rust".to_owned(),
            impl_version: impl_version.to_owned(),
            started_at: chrono::Utc::now(),
        }
    }

    /// The exact bytes to write to stdout, newline included.
    #[must_use]
    pub fn to_line(&self) -> String {
        // A handshake that cannot be serialized is a programming error, but
        // panicking here would leave the host hanging on a silent stdout until
        // its timeout. Fall back to a line the host will reject explicitly.
        match serde_json::to_string(self) {
            Ok(s) => format!("{s}\n"),
            Err(e) => format!("{{\"kind\":\"aum.handshake.error\",\"error\":\"{e}\"}}\n"),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn handshake_is_exactly_one_line() {
        let h = Handshake::new(51234, 4711, "1.0.0", "0.1.0");
        let line = h.to_line();
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn handshake_round_trips() {
        let h = Handshake::new(51234, 4711, "1.0.0", "0.1.0");
        let parsed: Handshake = serde_json::from_str(h.to_line().trim()).unwrap();
        assert_eq!(parsed.port, 51234);
        assert_eq!(parsed.kind, Handshake::KIND);
    }
}
