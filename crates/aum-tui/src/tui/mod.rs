//! The interactive view.
//!
//! Seven tabs over the same data the subcommands print. The terminal is put
//! into raw mode and an alternate screen, and **restored by a panic hook before
//! the panic message is printed** — a panic in raw mode otherwise leaves the
//! shell with no echo and no line discipline, and the backtrace unreadable on
//! top of it. That is a worse failure than whatever caused the panic.

mod data;
mod draw;
mod heatmap;

use std::io::Stdout;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::context::Context;
use crate::sort::{self, Labels, Sort};
use data::Data;

pub use data::Day;

/// How long between automatic refreshes while the interface is open.
///
/// The agents write continuously, so the numbers should move. Two seconds is
/// the ingest loop's own idle cadence — polling faster would only re-read the
/// same rows.
const REFRESH: Duration = Duration::from_secs(2);

/// How long to wait for a key before redrawing anyway.
const TICK: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Daily,
    Hourly,
    Models,
    Stats,
    Sessions,
    Apps,
}

impl Tab {
    pub const ALL: [Self; 7] = [
        Self::Overview,
        Self::Daily,
        Self::Hourly,
        Self::Models,
        Self::Stats,
        Self::Sessions,
        Self::Apps,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Daily => "Daily",
            Self::Hourly => "Hourly",
            Self::Models => "Models",
            Self::Stats => "Stats",
            Self::Sessions => "Sessions",
            Self::Apps => "Apps",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Daily => 1,
            Self::Hourly => 2,
            Self::Models => 3,
            Self::Stats => 4,
            Self::Sessions => 5,
            Self::Apps => 6,
        }
    }
}

/// The ingest running behind the interface, or the reason there is none.
///
/// Not an `Option<Arc<…>>`: the interface has to *say* which of the two it is.
/// The bug this replaced was an interface that re-read the database every two
/// seconds, printed a fresh timestamp, and never once looked at the transcripts
/// — so a stalled number and a current one were indistinguishable.
pub enum Ingest {
    Running {
        state: std::sync::Arc<tokio::sync::RwLock<aum_engine::IngestState>>,
        wake: std::sync::Arc<tokio::sync::Notify>,
    },
    /// `--no-sync`: report what is stored and read nothing.
    Disabled,
}

impl Ingest {
    async fn snapshot(&self) -> Option<aum_engine::IngestState> {
        match self {
            Self::Running { state, .. } => Some(*state.read().await),
            Self::Disabled => None,
        }
    }

    fn wake(&self) {
        if let Self::Running { wake, .. } = self {
            wake.notify_one();
        }
    }
}

pub struct App {
    pub tab: Tab,
    pub selected: usize,
    pub data: Data,
    pub help: bool,
    pub status: Option<String>,
    pub currency: &'static str,
    pub ingest: Ingest,
    /// What ingest had done as of the last redraw. Read from the shared state
    /// rather than assumed, so "checked 2s ago" is a report and not a promise.
    pub ingest_state: Option<aum_engine::IngestState>,
    /// Daily and hourly share one order: they are the same question at two
    /// resolutions, and `h` swaps between them mid-thought.
    pub bucket_sort: Sort,
    pub model_sort: Sort,
    last_refresh: Instant,
}

impl App {
    /// How many rows the current tab can scroll through.
    fn rows(&self) -> usize {
        match self.tab {
            Tab::Overview | Tab::Stats | Tab::Apps => 0,
            Tab::Daily => self.data.daily.len(),
            Tab::Hourly => self.data.hourly.len(),
            Tab::Models => self.data.models.len(),
            Tab::Sessions => self.data.sessions.len(),
        }
    }

    /// The order in force on the current tab, if it has one.
    pub const fn sort(&self) -> Option<Sort> {
        match self.tab {
            Tab::Daily | Tab::Hourly => Some(self.bucket_sort),
            Tab::Models => Some(self.model_sort),
            _ => None,
        }
    }

    /// Re-sort the current tab, and go back to the top of the new order.
    ///
    /// Returns what happened, or `None` on a tab with nothing to sort — the
    /// caller says so rather than letting the key press vanish silently.
    fn press_sort(&mut self, key: sort::Key) -> Option<Sort> {
        let slot = match self.tab {
            Tab::Daily | Tab::Hourly => &mut self.bucket_sort,
            Tab::Models => &mut self.model_sort,
            _ => return None,
        };
        *slot = slot.press(key);
        let now = *slot;
        self.selected = 0;
        Some(now)
    }

    fn move_selection(&mut self, delta: isize) {
        let rows = self.rows();
        if rows == 0 {
            return;
        }
        let current = self.selected.min(rows - 1) as isize;
        self.selected = (current + delta).clamp(0, rows as isize - 1) as usize;
    }

    fn switch(&mut self, delta: isize) {
        let count = Tab::ALL.len() as isize;
        let next = (self.tab.index() as isize + delta).rem_euclid(count) as usize;
        self.tab = Tab::ALL[next];
        self.selected = 0;
    }
}

/// Put the terminal back exactly as it was found.
///
/// Called from the panic hook as well as the normal exit path, so it must be
/// safe to run twice and must never itself panic.
fn restore() {
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    );
}

pub async fn run(
    ctx: &Context,
    filter: &aum_db::usage::Filter,
    label: &str,
    sync: bool,
) -> anyhow::Result<()> {
    // Installed before raw mode, so even a panic during setup leaves a usable
    // shell behind.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));

    // The ingest loop runs for as long as the interface is open. Without it the
    // interface re-reads a database nobody is writing to, which is what "the
    // numbers update on their own" used to mean here.
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let ingest = if sync {
        let engine = aum_engine::Engine::new(ctx.db.clone(), &ctx.home);
        let running = Ingest::Running {
            state: engine.state_handle(),
            wake: engine.wake_handle(),
        };
        tokio::spawn(engine.run(stop_rx));
        running
    } else {
        Ingest::Disabled
    };

    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, ctx, filter, label, ingest).await;

    // Told to stop, not waited for. A pass caught mid-file is dropped before it
    // commits, so its cursor never advances and the next run reads it again —
    // late, never lost. Waiting instead would hold the terminal in raw mode for
    // however long a file takes.
    let _ = stop.send(true);

    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    restore();
    let _ = terminal.show_cursor();

    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ctx: &Context,
    filter: &aum_db::usage::Filter,
    label: &str,
    ingest: Ingest,
) -> anyhow::Result<()> {
    let mut app = App {
        tab: Tab::Overview,
        selected: 0,
        data: Data::load(ctx, filter, label).await?,
        help: false,
        status: None,
        currency: ctx.currency_code(),
        ingest_state: ingest.snapshot().await,
        ingest,
        bucket_sort: Sort::NEWEST_FIRST,
        model_sort: Sort::LARGEST_FIRST,
        last_refresh: Instant::now(),
    };

    loop {
        terminal.draw(|frame| draw::draw(frame, &app))?;

        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if handle_key(&mut app, key, ctx, filter, label).await? {
                        return Ok(());
                    }
                }
                Event::Mouse(m) => match m.kind {
                    event::MouseEventKind::ScrollDown => app.move_selection(1),
                    event::MouseEventKind::ScrollUp => app.move_selection(-1),
                    _ => {}
                },
                _ => {}
            }
        }

        if app.last_refresh.elapsed() >= REFRESH {
            // A failed refresh keeps the numbers already on screen rather than
            // blanking them: stale-but-labelled beats empty.
            if let Ok(fresh) = Data::load(ctx, filter, label).await {
                app.data = fresh;
            }
            app.ingest_state = app.ingest.snapshot().await;
            app.last_refresh = Instant::now();
        }
    }
}

/// Returns `true` when the interface should exit.
async fn handle_key(
    app: &mut App,
    key: KeyEvent,
    ctx: &Context,
    filter: &aum_db::usage::Filter,
    label: &str,
) -> anyhow::Result<bool> {
    if app.help {
        app.help = false;
        return Ok(false);
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),
        KeyCode::Char('?') => app.help = true,

        KeyCode::Tab | KeyCode::Right => app.switch(1),
        KeyCode::BackTab | KeyCode::Left => app.switch(-1),

        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::PageDown => app.move_selection(10),
        KeyCode::PageUp => app.move_selection(-10),
        KeyCode::Home => app.selected = 0,
        KeyCode::End => app.selected = app.rows().saturating_sub(1),

        // Daily and hourly are the same question at two resolutions, so one key
        // toggles between them rather than making it a tab hunt.
        KeyCode::Char('h') => {
            app.tab = if app.tab == Tab::Hourly {
                Tab::Daily
            } else {
                Tab::Hourly
            };
            app.selected = 0;
        }

        // Two separate things, and the message says which happened. The screen
        // re-reads immediately; the transcripts are checked by the ingest loop,
        // which is asked to go now and lands within a moment.
        KeyCode::Char('r') => {
            app.ingest.wake();
            app.data = Data::load(ctx, filter, label).await?;
            app.ingest_state = app.ingest.snapshot().await;
            app.last_refresh = Instant::now();
            app.status = Some(match app.ingest {
                Ingest::Running { .. } => "re-read; checking transcripts".to_owned(),
                Ingest::Disabled => "re-read — started with --no-sync, so nothing is being \
                                     read from disk"
                    .to_owned(),
            });
        }

        KeyCode::Char('e') => {
            let path = export(app)?;
            app.status = Some(format!("exported to {path}"));
        }

        // Sorting. The same column again reverses it.
        KeyCode::Char('d') => sorted(app, sort::Key::Label),
        KeyCode::Char('n') => sorted(app, sort::Key::Requests),
        KeyCode::Char('t') => sorted(app, sort::Key::Tokens),
        KeyCode::Char('c') => sorted(app, sort::Key::Cost),

        KeyCode::Char('1') => app.tab = Tab::Overview,
        KeyCode::Char('2') => app.tab = Tab::Daily,
        KeyCode::Char('3') => app.tab = Tab::Hourly,
        KeyCode::Char('4') => app.tab = Tab::Models,
        KeyCode::Char('5') => app.tab = Tab::Stats,
        KeyCode::Char('6') => app.tab = Tab::Sessions,
        KeyCode::Char('7') => app.tab = Tab::Apps,

        _ => {}
    }
    Ok(false)
}

/// Apply a sort key and say what it did.
///
/// On a tab with nothing to sort the press is reported rather than swallowed:
/// a key that appears to do nothing is indistinguishable from a broken one.
fn sorted(app: &mut App, key: sort::Key) {
    let labels = if app.tab == Tab::Models {
        Labels::Name
    } else {
        Labels::Time
    };
    app.status = Some(match app.press_sort(key) {
        Some(sort) => sort.describe(labels).to_owned(),
        None => format!("nothing to sort on {}", app.tab.title()),
    });
}

/// Write the current view to a JSON file beside the working directory.
///
/// Metadata only — the same figures the tables show. There is no prompt or
/// response text anywhere in this application to export.
fn export(app: &App) -> anyhow::Result<String> {
    let name = format!(
        "aum-{}-{}.json",
        app.tab.title().to_lowercase(),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    // Written in the order shown, and saying which order that was: a file whose
    // rows are shuffled relative to the screen it came from is a small lie about
    // what was exported.
    let mut daily = sort::join(&app.data.daily, &app.data.daily_cost);
    sort::apply(&mut daily, app.bucket_sort);
    let mut models = sort::join_models(&app.data.models, &app.data.model_cost, "(not reported)");
    sort::apply(&mut models, app.model_sort);

    let body = serde_json::json!({
        "range": app.data.label,
        "exported_at": chrono::Local::now().to_rfc3339(),
        "order": {
            "daily": app.bucket_sort.describe(Labels::Time),
            "models": app.model_sort.describe(Labels::Name),
            "shown": app.sort().map(|s| s.describe(
                if app.tab == Tab::Models { Labels::Name } else { Labels::Time },
            )),
        },
        "totals": {
            "requests": app.data.totals.requests,
            "failed": app.data.failed,
            "total_tokens": app.data.totals.grand_total(),
            "input_side": app.data.totals.input_side(),
            "output": app.data.totals.output_total,
            "reasoning": app.data.totals.reasoning,
        },
        "daily": daily.iter().map(|r| serde_json::json!({
            "at": r.label, "requests": r.totals.requests, "tokens": r.totals.grand_total(),
        })).collect::<Vec<_>>(),
        "models": models.iter().map(|r| serde_json::json!({
            "model": r.label, "requests": r.totals.requests, "tokens": r.totals.grand_total(),
        })).collect::<Vec<_>>(),
        "note": "Metadata only. Prompt and response text are never recorded by this tool.",
    });
    std::fs::write(&name, serde_json::to_string_pretty(&body)?)?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn tabs_wrap_in_both_directions() {
        assert_eq!(Tab::ALL.len(), 7);
        assert_eq!(Tab::Overview.index(), 0);
        assert_eq!(Tab::Apps.index(), 6);

        // Wrapping means Left from the first tab lands on the last, which is
        // what every tabbed interface does and what fingers expect.
        let count = Tab::ALL.len() as isize;
        assert_eq!((0_isize - 1).rem_euclid(count), 6);
        assert_eq!((6_isize + 1).rem_euclid(count), 0);
    }

    #[test]
    fn every_tab_has_a_title_and_a_distinct_index() {
        let mut seen = std::collections::HashSet::new();
        for tab in Tab::ALL {
            assert!(!tab.title().is_empty());
            assert!(seen.insert(tab.index()), "{:?} has a duplicate index", tab);
        }
        assert_eq!(seen.len(), 7);
    }
}
