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

pub struct App {
    pub tab: Tab,
    pub selected: usize,
    pub data: Data,
    pub help: bool,
    pub status: Option<String>,
    pub currency: &'static str,
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

pub async fn run(ctx: &Context, filter: &aum_db::usage::Filter, label: &str) -> anyhow::Result<()> {
    // Installed before raw mode, so even a panic during setup leaves a usable
    // shell behind.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));

    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;

    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, ctx, filter, label).await;

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
) -> anyhow::Result<()> {
    let mut app = App {
        tab: Tab::Overview,
        selected: 0,
        data: Data::load(ctx, filter, label).await?,
        help: false,
        status: None,
        currency: ctx.currency_code(),
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

        KeyCode::Char('r') => {
            app.data = Data::load(ctx, filter, label).await?;
            app.last_refresh = Instant::now();
            app.status = Some("refreshed".to_owned());
        }

        KeyCode::Char('e') => {
            let path = export(app)?;
            app.status = Some(format!("exported to {path}"));
        }

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
    let body = serde_json::json!({
        "range": app.data.label,
        "exported_at": chrono::Local::now().to_rfc3339(),
        "totals": {
            "requests": app.data.totals.requests,
            "failed": app.data.failed,
            "total_tokens": app.data.totals.grand_total(),
            "input_side": app.data.totals.input_side(),
            "output": app.data.totals.output_total,
            "reasoning": app.data.totals.reasoning,
        },
        "daily": app.data.daily.iter().map(|(at, t)| serde_json::json!({
            "at": at, "requests": t.requests, "tokens": t.grand_total(),
        })).collect::<Vec<_>>(),
        "models": app.data.models.iter().map(|(m, t)| serde_json::json!({
            "model": m, "requests": t.requests, "tokens": t.grand_total(),
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
