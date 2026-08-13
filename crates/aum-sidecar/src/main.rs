//! The sidecar binary.
//!
//! Deliberately thin: read configuration from the environment, bind a port,
//! print one handshake line, serve. All logic lives in the library crates.
//!
//! Two invariants this file exists to uphold:
//!
//! * **stdout carries exactly one line, ever** — the handshake. Everything else
//!   goes to stderr. A stray `println!` here would corrupt the host's bootstrap,
//!   which is also why the host spawns the compiled binary rather than
//!   `cargo run` (cargo writes build progress to stdout).
//! * **The process does not outlive its host.** A hard crash of the desktop app
//!   must not leave a sidecar holding the database and, later, child agent
//!   processes that are still spending tokens.

use std::process::ExitCode;
use std::time::Duration;

use aum_contract::handshake::{
    ENV_ALLOWED_ORIGIN, ENV_DATA_DIR, ENV_PARENT_PID, ENV_TOKEN, Handshake,
};
use aum_server::AppState;

const IMPL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How often the orphan watchdog checks that the host is still alive.
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);
/// How often a heartbeat goes out, so a client can tell a quiet stream from a
/// half-open socket.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // stderr, never stdout: the host is parsing stdout for the handshake.
            tracing::error!(error = %e, "sidecar failed to start");
            eprintln!("aum-sidecar: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Structured logs to **stderr**. `stdout` is reserved for the handshake.
fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_env("AUM_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .json()
        .init();
}

async fn run() -> anyhow::Result<()> {
    let config = Config::from_env()?;

    let listener = aum_server::bind().await?;
    let port = listener.local_addr()?.port();

    // Storage is optional at this level on purpose. If the database cannot be
    // opened, the process still serves /v1/health and reports the failure, so
    // the desktop app can show a diagnosable error instead of a blank screen.
    // Kept alive for the whole function: dropping the sender would signal
    // shutdown, and `serve` below runs until the process is asked to stop.
    let mut ingest_shutdown = None;

    let data = match aum_db::open(&config.data_dir).await {
        Ok(db) => {
            let home = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("could not determine the home directory"))?;
            let engine = aum_engine::Engine::new(db.clone(), &home);
            let ingest = engine.state_handle();

            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            tokio::spawn(engine.run(shutdown_rx));
            ingest_shutdown = Some(shutdown_tx);

            Some(aum_server::DataHandle { db, ingest })
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                data_dir = %config.data_dir.display(),
                "could not open the capture database; usage will not be recorded"
            );
            None
        }
    };

    let state = AppState::with_data(
        config.token,
        config.allowed_origin,
        port,
        IMPL_VERSION.to_owned(),
        data,
    );

    // The handshake goes out only once the listener is actually bound, so the
    // host can connect immediately on receiving it without a retry loop.
    emit_handshake(port)?;

    tracing::info!(
        port,
        data_dir = %config.data_dir.display(),
        stream_epoch = %state.stream_epoch(),
        "sidecar listening on loopback"
    );

    tokio::spawn(heartbeat(state.clone()));
    tokio::spawn(watchdog(config.parent_pid));

    aum_server::serve(listener, state).await?;

    // Stop ingest before leaving, so a pass in flight finishes its transaction
    // rather than being cut off mid-write.
    if let Some(tx) = ingest_shutdown.take() {
        let _ = tx.send(true);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Everything that must be durable has been written by this point; from here
    // on there is nothing left to do but leave.
    //
    // Exit explicitly rather than returning. The orphan watchdog reads stdin via
    // `tokio::io::stdin()`, which does its work on a blocking thread that cannot
    // be cancelled — and the tokio runtime waits for blocking threads when it
    // drops. Since the host still holds the write end of that pipe, the read
    // never returns and the process would hang after a clean shutdown, leaving
    // the host to SIGKILL it after its grace period on every single quit.
    tracing::info!("sidecar stopped");
    std::process::exit(0);
}

fn emit_handshake(port: u16) -> anyhow::Result<()> {
    use std::io::Write as _;

    let handshake = Handshake::new(
        port,
        std::process::id(),
        aum_contract::CONTRACT_VERSION,
        IMPL_VERSION,
    );
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(handshake.to_line().as_bytes())?;
    // Flush explicitly: if the host is waiting on this line and we exit before
    // the buffer drains, it sees a handshake timeout instead of a clean start.
    stdout.flush()?;
    Ok(())
}

async fn heartbeat(state: AppState) {
    let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
    loop {
        ticker.tick().await;
        if state.subscriber_count() > 0 {
            state.publish(None, aum_contract::AgentEvent::Heartbeat { lag_ms: 0 });
        }
    }
}

/// Exit when the host does.
///
/// Two independent signals, because each misses a case the other catches:
///
/// * **stdin EOF** — the host holds the write end of our stdin pipe. If it dies
///   for any reason, including `SIGKILL`, the pipe closes and the read returns
///   zero. This catches everything except a host that deliberately closed stdin.
/// * **parent liveness** — polling for the recorded PID catches the case where
///   stdin was redirected from `/dev/null` (which reports EOF immediately) or
///   inherited by another process.
async fn watchdog(parent_pid: Option<u32>) {
    tokio::spawn(async {
        use tokio::io::AsyncReadExt as _;
        let mut buf = [0_u8; 64];
        let mut stdin = tokio::io::stdin();
        loop {
            match stdin.read(&mut buf).await {
                // EOF: the host is gone.
                Ok(0) => {
                    tracing::warn!("stdin closed; host has exited, shutting down");
                    std::process::exit(0);
                }
                // The host does not normally write to us; ignore anything it does.
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(error = %e, "stdin watchdog stopped");
                    return;
                }
            }
        }
    });

    let Some(pid) = parent_pid else {
        tracing::debug!("no parent pid supplied; relying on stdin EOF alone");
        return;
    };

    let target = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    let mut ticker = tokio::time::interval(WATCHDOG_INTERVAL);
    loop {
        ticker.tick().await;
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
        if system.process(target).is_none() {
            tracing::warn!(parent_pid = pid, "host process is gone, shutting down");
            std::process::exit(0);
        }
    }
}

struct Config {
    token: String,
    allowed_origin: String,
    data_dir: std::path::PathBuf,
    parent_pid: Option<u32>,
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        // The token arrives in the environment, never in argv: argv is
        // world-readable through `ps aux`, so a token passed there would be
        // visible to every process on the machine.
        let token = std::env::var(ENV_TOKEN).map_err(|_| {
            anyhow::anyhow!(
                "{ENV_TOKEN} is not set. The host must generate a token and pass it in the \
                 environment; it is deliberately not accepted as a command-line argument."
            )
        })?;

        anyhow::ensure!(
            token.len() >= 32,
            "{ENV_TOKEN} is too short ({} chars); expected at least 32",
            token.len()
        );

        let allowed_origin =
            std::env::var(ENV_ALLOWED_ORIGIN).unwrap_or_else(|_| "app://local".to_owned());

        let data_dir = match std::env::var(ENV_DATA_DIR) {
            Ok(d) => std::path::PathBuf::from(d),
            Err(_) => default_data_dir()?,
        };
        std::fs::create_dir_all(&data_dir)?;

        let parent_pid = std::env::var(ENV_PARENT_PID)
            .ok()
            .and_then(|v| v.parse::<u32>().ok());

        Ok(Self {
            token,
            allowed_origin,
            data_dir,
            parent_pid,
        })
    }
}

fn default_data_dir() -> anyhow::Result<std::path::PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("could not determine the OS data directory"))?;
    Ok(base.join("agent-usage-monitor"))
}
