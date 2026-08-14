//! Opening the database and catching up on transcripts.
//!
//! What the old sidecar's startup did, minus everything that existed because a
//! desktop application was supervising it: no token, no port, no handshake, no
//! parent-process watchdog. Just a file and a directory to read.

use std::path::{Path, PathBuf};

use aum_engine::Engine;
use aum_engine::prices::MoneyState;

pub struct Context {
    pub db: aum_db::Database,
    pub money: MoneyState,
    pub home: PathBuf,
    pub currency: aum_contract::Currency,
    pub colour: bool,
}

/// Where the capture database lives.
///
/// `AUM_DATA_DIR` overrides it, which is what makes a scratch database possible
/// for anyone poking at this without touching their real history.
#[must_use]
pub fn default_data_dir() -> PathBuf {
    std::env::var_os("AUM_DATA_DIR").map_or_else(
        || {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("agent-usage-monitor")
        },
        PathBuf::from,
    )
}

impl Context {
    /// Open everything a report needs.
    ///
    /// `sync` reads whatever the agents have written since last time. It is on
    /// by default because a monitor that reports yesterday's numbers without
    /// saying so is worse than one that takes a moment; `--no-sync` is there for
    /// when the answer needs to be instant or the transcripts are unreadable.
    pub async fn open(
        db_path: Option<&Path>,
        currency: &str,
        colour: bool,
        sync: bool,
    ) -> anyhow::Result<Self> {
        let currency = aum_engine::prices::parse_currency(currency).ok_or_else(|| {
            anyhow::anyhow!("{currency} is not a currency this tool can present (USD, EUR, CZK)")
        })?;

        let db = match db_path {
            Some(p) => aum_db::open_at(p).await?,
            None => aum_db::open(&default_data_dir()).await?,
        };

        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("this account has no home directory to read"))?;

        if sync {
            // One pass, foreground. Cursors mean it reads only what is new, so
            // on a warm database this is a few dozen `stat` calls.
            let engine = Engine::new(db.clone(), &home);
            let stats = engine.pass().await;
            tracing::debug!(
                files = stats.files_scanned,
                new = stats.usage_recorded,
                "caught up"
            );
        }

        let money = MoneyState::load(&db).await;

        Ok(Self {
            db,
            money,
            home,
            currency,
            colour,
        })
    }

    #[must_use]
    pub fn cost(&self) -> aum_engine::prices::CostContext<'_> {
        self.money.cost_context(self.currency)
    }

    #[must_use]
    pub fn currency_code(&self) -> &'static str {
        self.currency.code()
    }
}
