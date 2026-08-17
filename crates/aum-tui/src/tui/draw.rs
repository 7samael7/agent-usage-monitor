//! Drawing the tabs.
//!
//! Colour reinforces the certainty prefixes rather than replacing them, exactly
//! as in the plain-text output: every figure still carries `≈`, `≥` or `—`, so
//! a screenshot in greyscale says the same thing as the live screen.

use aum_contract::{DisplayKind, Measured, Money};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Bar, BarChart, BarGroup, Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table, Tabs,
    Wrap,
};

use super::heatmap;
use super::{App, Tab};
use crate::fmt;
use crate::sort::Key as SortKey;

const ACCENT: Color = Color::Indexed(110);
const MUTED: Color = Color::Indexed(244);
const FAINT: Color = Color::Indexed(240);

/// What a model with no reported id is called, in one place: the Overview and
/// Models tables have to agree, and the sorter needs the same string.
const NOT_REPORTED: &str = "(not reported)";

const fn kind_colour(kind: DisplayKind) -> Color {
    match kind {
        DisplayKind::Exact => Color::Indexed(114),
        DisplayKind::Calculated => ACCENT,
        DisplayKind::Estimated | DisplayKind::Partial => Color::Indexed(179),
        DisplayKind::Unavailable => MUTED,
    }
}

fn tokens_span(m: &Measured<u64>) -> Span<'static> {
    let kind = m.accuracy.display_kind();
    Span::styled(
        fmt::render_tokens(m, false),
        Style::default().fg(kind_colour(kind)),
    )
}

fn money_span(m: &Measured<Money>, currency: &str) -> Span<'static> {
    let kind = m.accuracy.display_kind();
    Span::styled(
        fmt::render_money(m, currency, false),
        Style::default().fg(kind_colour(kind)),
    )
}

pub fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tabs
            Constraint::Min(3),    // body
            Constraint::Length(1), // status
        ])
        .split(frame.area());

    draw_tabs(frame, chunks[0], app);

    match app.tab {
        Tab::Overview => overview(frame, chunks[1], app),
        Tab::Daily => buckets(frame, chunks[1], app, false),
        Tab::Hourly => buckets(frame, chunks[1], app, true),
        Tab::Models => models(frame, chunks[1], app),
        Tab::Stats => stats(frame, chunks[1], app),
        Tab::Sessions => sessions(frame, chunks[1], app),
        Tab::Apps => apps(frame, chunks[1], app),
    }

    draw_status(frame, chunks[2], app);

    if app.help {
        help(frame, frame.area());
    }
}

fn draw_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, t)| Line::from(format!(" {} {} ", i + 1, t.title())))
        .collect();
    let selected = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .style(Style::default().fg(MUTED))
            .highlight_style(
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            )
            .divider(""),
        area,
    );
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![
        Span::styled(
            format!(" {} ", app.data.label),
            Style::default().fg(Color::White).bg(FAINT),
        ),
        Span::raw("  "),
        Span::styled(
            "exact · ≈ calculated · ≥ at least · — not measured",
            Style::default().fg(MUTED),
        ),
    ];
    if let Some(status) = &app.status {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(status.clone(), Style::default().fg(ACCENT)));
    }

    // Two clocks, because they can disagree and the difference is the whole
    // question. `read` is when the screen last queried the database; `checked`
    // is when the transcripts were last looked at. A frozen number with a
    // ticking clock beside it was this interface's most convincing bug.
    let (checked, tone) = ingest_age(app.ingest_state, chrono::Utc::now());
    spans.push(Span::styled(
        format!("   read {}", app.data.loaded_at.format("%H:%M:%S")),
        Style::default().fg(FAINT),
    ));
    spans.push(Span::styled(
        format!(" · {checked}"),
        Style::default().fg(tone),
    ));
    spans.push(Span::styled(
        "   ? help   q quit",
        Style::default().fg(FAINT),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// How current the underlying data is, in words.
///
/// Takes the state rather than the whole `App`, and takes `now` rather than
/// reading the clock, so what it says can be tested.
fn ingest_age(
    state: Option<aum_engine::IngestState>,
    now: chrono::DateTime<chrono::Utc>,
) -> (String, Color) {
    let Some(state) = state else {
        return ("not reading transcripts (--no-sync)".to_owned(), MUTED);
    };
    if state.backfilling {
        return ("reading history…".to_owned(), Color::Indexed(179));
    }
    let Some(at) = state.last_pass_at else {
        return ("not checked yet".to_owned(), Color::Indexed(179));
    };
    let seconds = (now - at).num_seconds().max(0);
    let phrase = match seconds {
        0..=4 => "checked just now".to_owned(),
        5..=90 => format!("checked {seconds}s ago"),
        _ => format!("checked {}m ago", seconds / 60),
    };
    // Ingest polls every two seconds at its slowest, so a minute of silence
    // means something is wrong and the figures should stop looking current.
    let tone = if seconds > 60 {
        Color::Indexed(179)
    } else {
        FAINT
    };
    (phrase, tone)
}

fn block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(FAINT))
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
}

// ── Overview ────────────────────────────────────────────────────────────────

fn overview(frame: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),
            Constraint::Min(6),
            Constraint::Length(12),
        ])
        .split(area);

    let t = &app.data.totals;
    let reasoning = super::super::report::reasoning_measure(t);
    let mut lines = vec![
        stat_line("requests", &fmt::thousands(t.requests)),
        stat_line("total tokens", &fmt::thousands(t.grand_total())),
        stat_line("input side", &fmt::thousands(t.input_side())),
        stat_line("output", &fmt::thousands(t.output_total)),
        Line::from(vec![
            Span::styled(format!("  {:<16}", "reasoning"), Style::default().fg(MUTED)),
            tokens_span(&reasoning),
        ]),
        Line::from(vec![
            Span::styled(
                format!("  {:<16}", "API-equivalent"),
                Style::default().fg(MUTED),
            ),
            money_span(&app.data.cost, app.currency),
        ]),
        Line::from(vec![
            Span::styled(
                format!("  {:<16}", "actually billed"),
                Style::default().fg(MUTED),
            ),
            Span::styled(
                "—  subscription, not billed per token",
                Style::default().fg(MUTED),
            ),
        ]),
    ];
    if !app.data.adapters.is_empty() {
        let by_agent = app
            .data
            .adapters
            .iter()
            .map(|(a, t)| format!("{a} {}", fmt::thousands(t.grand_total())))
            .collect::<Vec<_>>()
            .join("   ");
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<16}", "by agent"), Style::default().fg(MUTED)),
            Span::raw(by_agent),
        ]));
    }
    if app.data.failed > 0 {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<16}", "unmeasured"),
                Style::default().fg(MUTED),
            ),
            Span::styled(
                format!(
                    "{} request(s) failed and produced no tokens",
                    fmt::thousands(app.data.failed)
                ),
                Style::default().fg(Color::Indexed(179)),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).block(block("Totals")), rows[0]);

    daily_chart(frame, rows[1], app, "Tokens per day");

    let mut table_rows: Vec<Row> = Vec::new();
    for (i, (model, totals)) in app.data.models.iter().take(9).enumerate() {
        let cost = app.data.model_cost.get(i).cloned().unwrap_or_else(|| {
            Measured::unavailable(aum_contract::UnavailableReason::NoTelemetry {
                detail: "not costed".to_owned(),
            })
        });
        table_rows.push(Row::new(vec![
            Cell::from(model.clone().unwrap_or_else(|| NOT_REPORTED.to_owned())),
            Cell::from(fmt::thousands(totals.requests)).style(Style::default().fg(MUTED)),
            Cell::from(fmt::thousands(totals.grand_total())),
            Cell::from(Line::from(money_span(&cost, app.currency)).alignment(Alignment::Right)),
        ]));
    }
    frame.render_widget(
        Table::new(
            table_rows,
            [
                Constraint::Length(28),
                Constraint::Length(10),
                Constraint::Length(16),
                Constraint::Length(14),
                Constraint::Min(0),
            ],
        )
        .header(header_row(&["model", "requests", "tokens", "cost", ""]))
        .block(block("Busiest models")),
        rows[2],
    );
}

fn stat_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<16}"), Style::default().fg(MUTED)),
        Span::raw(value.to_owned()),
    ])
}

/// A header where the sorted column carries its direction.
///
/// The marker is on the column rather than only in the title, because the
/// question "sorted by what?" is asked while looking at the columns.
fn sorted_header(cells: &[(&str, Option<SortKey>)], sort: crate::sort::Sort) -> Row<'static> {
    let labelled: Vec<String> = cells
        .iter()
        .map(|(name, key)| match key {
            Some(k) => format!("{name}{}", sort.marker(*k)),
            None => (*name).to_owned(),
        })
        .collect();
    Row::new(labelled.into_iter().map(Cell::from).collect::<Vec<_>>())
        .style(Style::default().fg(FAINT).add_modifier(Modifier::BOLD))
}

fn header_row(cells: &[&str]) -> Row<'static> {
    Row::new(
        cells
            .iter()
            .map(|c| Cell::from((*c).to_owned()))
            .collect::<Vec<_>>(),
    )
    .style(Style::default().fg(FAINT).add_modifier(Modifier::BOLD))
}

// ── Daily / hourly ──────────────────────────────────────────────────────────

/// The span a chart is showing, for its title — the bar labels are day numbers
/// and cannot say which month they belong to.
fn span_of(series: &[&(String, aum_db::usage::Totals)]) -> String {
    match (series.first(), series.last()) {
        (Some((a, _)), Some((b, _))) if a == b => format!(" · {a}"),
        (Some((a, _)), Some((b, _))) => format!(" · {a} to {b}"),
        _ => String::new(),
    }
}

fn daily_chart(frame: &mut Frame, area: Rect, app: &App, title: &str) {
    let width = area.width.saturating_sub(4) as usize;
    // Four columns per bar including the gap, so the chart shows as many days
    // as fit rather than squeezing a year into a smear.
    let capacity = (width / 4).max(1);
    let series: Vec<&(String, aum_db::usage::Totals)> =
        app.data.daily.iter().rev().take(capacity).rev().collect();

    if series.is_empty() {
        frame.render_widget(
            Paragraph::new("Nothing recorded in this range.")
                .style(Style::default().fg(MUTED))
                .block(block(title)),
            area,
        );
        return;
    }

    let bars: Vec<Bar> = series
        .iter()
        .map(|(at, totals)| {
            let value = u64::try_from(totals.grand_total()).unwrap_or(0);
            Bar::default()
                .value(value)
                .label(Line::from(at.get(8..).unwrap_or(at).to_owned()))
                .text_value(compact(totals.grand_total()))
                .style(Style::default().fg(ACCENT))
        })
        .collect();

    frame.render_widget(
        BarChart::default()
            .data(BarGroup::default().bars(&bars))
            .bar_width(3)
            .bar_gap(1)
            .value_style(Style::default().fg(Color::Black).bg(ACCENT))
            .label_style(Style::default().fg(MUTED))
            .block(block(&format!("{title}{}", span_of(&series)))),
        area,
    );
}

/// A compact token count for a bar label, where three characters is all there is.
fn compact(n: i64) -> String {
    // Every tier up to exa, so the label cannot outgrow the bar it sits in
    // whatever the number. Real counts stop at G today, but a label that
    // overflows its column corrupts every bar beside it.
    match n {
        0 => "0".to_owned(),
        n if n >= 1_000_000_000_000_000_000 => format!("{}E", n / 1_000_000_000_000_000_000),
        n if n >= 1_000_000_000_000_000 => format!("{}P", n / 1_000_000_000_000_000),
        n if n >= 1_000_000_000_000 => format!("{}T", n / 1_000_000_000_000),
        n if n >= 1_000_000_000 => format!("{}G", n / 1_000_000_000),
        n if n >= 1_000_000 => format!("{}M", n / 1_000_000),
        n if n >= 1_000 => format!("{}k", n / 1_000),
        n => n.to_string(),
    }
}

fn buckets(frame: &mut Frame, area: Rect, app: &App, hourly: bool) {
    let (series, costs, title) = if hourly {
        (&app.data.hourly, &app.data.hourly_cost, "Per hour")
    } else {
        (&app.data.daily, &app.data.daily_cost, "Per day")
    };

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Min(5)])
        .split(area);

    if hourly {
        hourly_chart(frame, split[0], app);
    } else {
        daily_chart(frame, split[0], app, "Tokens per day");
    }

    // The chart above stays in time order whatever the table does: a bar chart
    // with the days shuffled is not a smaller truth, it is a different chart.
    let sort = app.bucket_sort;
    let mut ordered = crate::sort::join(series, costs);
    crate::sort::apply(&mut ordered, sort);

    let unmeasured = Measured::unavailable(aum_contract::UnavailableReason::NoTelemetry {
        detail: "no usage".to_owned(),
    });

    let rows: Vec<Row> = ordered
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let style = if i == app.selected.min(ordered.len().saturating_sub(1)) {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(r.label.replace('T', " ")),
                Cell::from(fmt::thousands(r.totals.requests)),
                Cell::from(fmt::thousands(r.totals.input_side())),
                Cell::from(fmt::thousands(r.totals.output_total)),
                Cell::from(fmt::thousands(r.totals.grand_total())),
                Cell::from(
                    Line::from(money_span(r.cost.unwrap_or(&unmeasured), app.currency))
                        .alignment(Alignment::Right),
                ),
            ])
            .style(style)
        })
        .collect();

    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(18),
                Constraint::Length(10),
                Constraint::Length(16),
                Constraint::Length(12),
                Constraint::Length(16),
                Constraint::Length(14),
            ],
        )
        .header(sorted_header(
            &[
                (if hourly { "hour" } else { "day" }, Some(SortKey::Label)),
                ("requests", Some(SortKey::Requests)),
                // `input` and `output` are parts of the quantity `tokens`
                // sorts on, so they carry no marker of their own.
                ("input", None),
                ("output", None),
                ("tokens", Some(SortKey::Tokens)),
                ("cost", Some(SortKey::Cost)),
            ],
            sort,
        ))
        .block(block(&format!(
            "{title} — {}",
            sort.describe(crate::sort::Labels::Time)
        ))),
        split[1],
    );
}

fn hourly_chart(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width.saturating_sub(4) as usize;
    let capacity = (width / 4).max(1);
    let series: Vec<&(String, aum_db::usage::Totals)> =
        app.data.hourly.iter().rev().take(capacity).rev().collect();

    if series.is_empty() {
        frame.render_widget(
            Paragraph::new("Nothing recorded in this range.")
                .style(Style::default().fg(MUTED))
                .block(block("Tokens per hour")),
            area,
        );
        return;
    }

    let bars: Vec<Bar> = series
        .iter()
        .map(|(at, totals)| {
            Bar::default()
                .value(u64::try_from(totals.grand_total()).unwrap_or(0))
                .label(Line::from(
                    at.split('T').next_back().unwrap_or("").to_owned(),
                ))
                .text_value(compact(totals.grand_total()))
                .style(Style::default().fg(ACCENT))
        })
        .collect();

    frame.render_widget(
        BarChart::default()
            .data(BarGroup::default().bars(&bars))
            .bar_width(3)
            .bar_gap(1)
            .value_style(Style::default().fg(Color::Black).bg(ACCENT))
            .label_style(Style::default().fg(MUTED))
            .block(block(&format!("Tokens per hour{}", span_of(&series)))),
        area,
    );
}

// ── Models ──────────────────────────────────────────────────────────────────

fn models(frame: &mut Frame, area: Rect, app: &App) {
    let unpriced: Vec<&str> = app
        .data
        .models
        .iter()
        .filter_map(|(m, _)| m.as_deref())
        .filter(|m| !app.data.applications.is_empty() && !priced(app, m))
        .collect();

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(if unpriced.is_empty() { 0 } else { 5 }),
        ])
        .split(area);

    let sort = app.model_sort;
    let mut ordered =
        crate::sort::join_models(&app.data.models, &app.data.model_cost, NOT_REPORTED);
    crate::sort::apply(&mut ordered, sort);

    let uncosted = Measured::unavailable(aum_contract::UnavailableReason::NoTelemetry {
        detail: "not costed".to_owned(),
    });

    let rows: Vec<Row> = ordered
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let reasoning = super::super::report::reasoning_measure(r.totals);
            let style = if i == app.selected.min(ordered.len().saturating_sub(1)) {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(r.label.to_owned()),
                Cell::from(fmt::thousands(r.totals.requests)),
                Cell::from(fmt::thousands(r.totals.grand_total())),
                Cell::from(Line::from(tokens_span(&reasoning)).alignment(Alignment::Right)),
                Cell::from(
                    Line::from(money_span(r.cost.unwrap_or(&uncosted), app.currency))
                        .alignment(Alignment::Right),
                ),
            ])
            .style(style)
        })
        .collect();

    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(28),
                Constraint::Length(10),
                Constraint::Length(16),
                Constraint::Length(14),
                Constraint::Length(14),
                Constraint::Min(0),
            ],
        )
        .header(sorted_header(
            &[
                ("model", Some(SortKey::Label)),
                ("requests", Some(SortKey::Requests)),
                ("tokens", Some(SortKey::Tokens)),
                ("reasoning", None),
                ("cost", Some(SortKey::Cost)),
                ("", None),
            ],
            sort,
        ))
        .block(block(&format!(
            "Models — {}",
            sort.describe(crate::sort::Labels::Name)
        ))),
        split[0],
    );

    if !unpriced.is_empty() {
        let mut lines = vec![Line::from(Span::styled(
            format!(
                "{} model(s) have no price, so their cost reads — rather than as zero:",
                unpriced.len()
            ),
            Style::default().fg(Color::Indexed(179)),
        ))];
        for m in unpriced.iter().take(3) {
            lines.push(Line::from(Span::styled(
                format!("  aum price {m} --input <rate> --output <rate>"),
                Style::default().fg(MUTED),
            )));
        }
        frame.render_widget(Paragraph::new(lines).block(block("Unpriced")), split[1]);
    }
}

fn priced(app: &App, model: &str) -> bool {
    app.data
        .models
        .iter()
        .position(|(m, _)| m.as_deref() == Some(model))
        .and_then(|i| app.data.model_cost.get(i))
        .is_some_and(|c| c.value.is_some())
}

// ── Stats: the contribution graph ───────────────────────────────────────────

fn stats(frame: &mut Frame, area: Rect, app: &App) {
    let scale = heatmap::Scale::of(&app.data.calendar);

    // Weeks run down the columns, so seven rows plus a header and a legend.
    let mut weeks: Vec<Vec<&super::Day>> = Vec::new();
    let mut current: Vec<&super::Day> = Vec::new();
    for day in &app.data.calendar {
        use chrono::Datelike as _;
        if day.date.weekday() == chrono::Weekday::Mon && !current.is_empty() {
            weeks.push(std::mem::take(&mut current));
        }
        current.push(day);
    }
    if !current.is_empty() {
        weeks.push(current);
    }

    let width = area.width.saturating_sub(8) as usize;
    let visible: Vec<&Vec<&super::Day>> = weeks.iter().rev().take(width.max(1)).rev().collect();

    // Every other weekday is labelled; seven labels in seven rows is noise.
    let labels = ["Mon", "", "Wed", "", "Fri", "", "Sun"];
    let mut lines: Vec<Line> = Vec::new();
    for (weekday, label) in labels.iter().enumerate() {
        let mut spans = vec![Span::styled(
            format!("{label:<4}"),
            Style::default().fg(FAINT),
        )];
        for week in &visible {
            let cell = week.iter().find(|d| {
                use chrono::Datelike as _;
                d.date.weekday().num_days_from_monday() as usize == weekday
            });
            match cell {
                Some(day) => {
                    let level = scale.level(day.tokens);
                    spans.push(Span::styled(
                        heatmap::glyph(level).to_owned(),
                        Style::default().fg(heatmap::colour(level)),
                    ));
                }
                None => spans.push(Span::raw(" ")),
            }
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::from(""));
    let active = app.data.calendar.iter().filter(|d| d.tokens > 0).count();
    lines.push(Line::from(vec![
        Span::styled("    less ", Style::default().fg(FAINT)),
        Span::styled(heatmap::glyph(0), Style::default().fg(heatmap::colour(0))),
        Span::styled(heatmap::glyph(1), Style::default().fg(heatmap::colour(1))),
        Span::styled(heatmap::glyph(2), Style::default().fg(heatmap::colour(2))),
        Span::styled(heatmap::glyph(3), Style::default().fg(heatmap::colour(3))),
        Span::styled(heatmap::glyph(4), Style::default().fg(heatmap::colour(4))),
        Span::styled(" more", Style::default().fg(FAINT)),
        Span::styled(
            format!(
                "     {active} active day(s) · {} requests · busiest day {} tokens",
                fmt::thousands(app.data.calendar.iter().map(|d| d.requests).sum::<i64>()),
                fmt::thousands(scale.peak)
            ),
            Style::default().fg(MUTED),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "    Shade is a quartile of the days that had usage, not a linear scale — a year spans \
         several orders of magnitude.",
        Style::default().fg(FAINT),
    )));

    frame.render_widget(
        Paragraph::new(lines).block(block("A year of activity")),
        area,
    );
}

// ── Sessions ────────────────────────────────────────────────────────────────

fn sessions(frame: &mut Frame, area: Rect, app: &App) {
    let rows: Vec<Row> = app
        .data
        .sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let style = if i == app.selected.min(app.data.sessions.len().saturating_sub(1)) {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(s.last_at.as_deref().map_or_else(
                    || "—".to_owned(),
                    |a| a.replace('T', " ").chars().take(16).collect(),
                )),
                Cell::from(s.adapter_id.clone()),
                Cell::from(s.model_id.clone().unwrap_or_else(|| "—".to_owned())),
                Cell::from(if s.failed > 0 {
                    Line::from(vec![
                        Span::raw(fmt::thousands(i64::from(s.requests))),
                        Span::styled(
                            format!(" +{} failed", s.failed),
                            Style::default().fg(Color::Indexed(179)),
                        ),
                    ])
                } else {
                    Line::from(fmt::thousands(i64::from(s.requests)))
                }),
                Cell::from(Line::from(tokens_span(&s.total_tokens)).alignment(Alignment::Right)),
                Cell::from(
                    Line::from(tokens_span(&s.reasoning_tokens)).alignment(Alignment::Right),
                ),
            ])
            .style(style)
        })
        .collect();

    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(18),
                Constraint::Length(13),
                Constraint::Length(28),
                Constraint::Length(18),
                Constraint::Length(16),
                Constraint::Length(14),
                Constraint::Min(0),
            ],
        )
        .header(header_row(&[
            "last active",
            "agent",
            "model",
            "requests",
            "tokens",
            "reasoning",
            "",
        ]))
        .block(block("Recent sessions")),
        area,
    );
}

// ── Applications ────────────────────────────────────────────────────────────

fn apps(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();
    for d in &app.data.applications {
        lines.push(Line::from(vec![
            Span::styled(
                d.display_name.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("{:?}", d.state).to_lowercase(),
                Style::default().fg(MUTED),
            ),
        ]));
        if let Some(path) = &d.executable_path {
            lines.push(Line::from(Span::styled(
                format!("  {path}"),
                Style::default().fg(FAINT),
            )));
        }
        for (name, state) in &d.capabilities {
            let (mark, colour, detail) = match state {
                aum_contract::CapabilityState::Supported { evidence } => {
                    ("yes", Color::Indexed(114), evidence.as_str())
                }
                aum_contract::CapabilityState::Degraded { caveat, .. } => {
                    ("~  ", Color::Indexed(179), caveat.as_str())
                }
                aum_contract::CapabilityState::Unsupported { reason } => {
                    ("no ", MUTED, reason.as_str())
                }
                aum_contract::CapabilityState::Unknown { reason } => {
                    ("?  ", FAINT, reason.as_str())
                }
            };
            lines.push(Line::from(vec![
                Span::styled(format!("    {mark}  "), Style::default().fg(colour)),
                Span::styled(format!("{name:<28}"), Style::default().fg(MUTED)),
                Span::styled(
                    detail.chars().take(70).collect::<String>(),
                    Style::default().fg(FAINT),
                ),
            ]));
        }
        if let Some(total) = &d.daily_total {
            lines.push(Line::from(vec![
                Span::styled("    daily total  ", Style::default().fg(Color::White)),
                tokens_span(&total.tokens),
                Span::styled(format!("  on {}", total.day), Style::default().fg(MUTED)),
            ]));
            lines.push(Line::from(Span::styled(
                format!("      {}", total.scope),
                Style::default().fg(FAINT),
            )));
        }
        lines.push(Line::from(""));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block("What each application can report")),
        area,
    );
}

// ── Help ────────────────────────────────────────────────────────────────────

fn help(frame: &mut Frame, area: Rect) {
    let width = 62.min(area.width.saturating_sub(4));
    let height = 22.min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let lines = vec![
        Line::from(""),
        key_line("Tab  →  ←", "move between tabs"),
        key_line("1 … 7", "jump straight to a tab"),
        key_line("↑ ↓  j k", "move through rows"),
        key_line("PgUp PgDn", "move a screen at a time"),
        key_line("h", "swap daily and hourly"),
        key_line("r", "re-read, and check the transcripts now"),
        key_line("e", "export this view as JSON"),
        key_line("?", "this help"),
        key_line("q  Esc", "quit"),
        Line::from(""),
        Line::from(Span::styled(
            "  Sorting — daily, hourly and models",
            Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
        )),
        key_line("d", "by date, or by model name"),
        key_line("n", "by number of requests"),
        key_line("t", "by tokens"),
        key_line("c", "by cost — unpriced rows stay last"),
        Line::from(Span::styled(
            "  The same key again reverses it. The chart stays in time order.",
            Style::default().fg(FAINT),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Transcripts are read while this is open; the screen re-reads every",
            Style::default().fg(MUTED),
        )),
        Line::from(Span::styled(
            "  two seconds. The bar below says when each last happened.",
            Style::default().fg(MUTED),
        )),
        Line::from(Span::styled(
            "  Any key closes this.",
            Style::default().fg(FAINT),
        )),
    ];

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .title(" Keys "),
        ),
        popup,
    );
}

fn key_line(keys: &str, what: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {keys:<12}"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(what.to_owned(), Style::default().fg(MUTED)),
    ])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn at(
        seconds_ago: i64,
    ) -> (
        Option<aum_engine::IngestState>,
        chrono::DateTime<chrono::Utc>,
    ) {
        let now = chrono::Utc::now();
        (
            Some(aum_engine::IngestState {
                passes: 1,
                last_pass_at: Some(now - chrono::Duration::seconds(seconds_ago)),
                ..Default::default()
            }),
            now,
        )
    }

    #[test]
    fn an_interface_that_is_not_reading_transcripts_says_so() {
        // The bug this replaced: no ingest at all, and a status bar showing a
        // fresh timestamp every two seconds because the *screen* had redrawn.
        // Silence here is the failure mode, so there is no silent case.
        let (text, _) = ingest_age(None, chrono::Utc::now());
        assert!(text.contains("not reading"), "{text}");
        assert!(text.contains("--no-sync"), "and why: {text}");
    }

    #[test]
    fn an_empty_screen_during_the_first_pass_is_labelled_rather_than_believed() {
        let now = chrono::Utc::now();
        let (text, _) = ingest_age(
            Some(aum_engine::IngestState {
                backfilling: true,
                ..Default::default()
            }),
            now,
        );
        assert!(text.contains("reading history"), "{text}");
    }

    #[test]
    fn a_pass_that_has_never_finished_is_not_reported_as_recent() {
        let (text, _) = ingest_age(Some(aum_engine::IngestState::default()), chrono::Utc::now());
        assert!(text.contains("not checked"), "{text}");
    }

    #[test]
    fn the_age_is_reported_in_units_that_stay_short() {
        let (fresh, _) = ingest_age(at(1).0, at(1).1);
        assert_eq!(fresh, "checked just now");
        let (state, now) = at(30);
        assert_eq!(ingest_age(state, now).0, "checked 30s ago");
        let (state, now) = at(605);
        assert_eq!(ingest_age(state, now).0, "checked 10m ago");
    }

    #[test]
    fn a_stalled_ingest_stops_looking_ordinary() {
        // Ingest polls every two seconds at its slowest. A minute of silence
        // means something is wrong, and the figures on screen are older than
        // they look — so the colour changes rather than the user having to
        // notice a number creeping up.
        let (state, now) = at(5);
        let (_, ordinary) = ingest_age(state, now);
        let (state, now) = at(300);
        let (_, stalled) = ingest_age(state, now);
        assert_ne!(ordinary, stalled);
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_produce_a_negative_age() {
        let now = chrono::Utc::now();
        let (text, _) = ingest_age(
            Some(aum_engine::IngestState {
                passes: 1,
                last_pass_at: Some(now + chrono::Duration::seconds(30)),
                ..Default::default()
            }),
            now,
        );
        assert_eq!(text, "checked just now", "{text}");
    }

    #[test]
    fn compact_counts_stay_short_enough_for_a_bar_label() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1_500), "1k");
        assert_eq!(compact(2_400_000), "2M");
        assert_eq!(compact(8_716_763_908), "8G");
        for n in [0, 1, 999, 1_000, 1_000_000, i64::MAX] {
            assert!(compact(n).len() <= 4, "{n} rendered as {}", compact(n));
        }
    }

    #[test]
    fn every_certainty_has_its_own_colour() {
        let all = [
            DisplayKind::Exact,
            DisplayKind::Calculated,
            DisplayKind::Estimated,
            DisplayKind::Partial,
            DisplayKind::Unavailable,
        ];
        // Estimated and Partial deliberately share one; the prefix separates
        // them, and a terminal has few usable shades.
        let distinct: std::collections::HashSet<_> = all.iter().map(|k| kind_colour(*k)).collect();
        assert!(distinct.len() >= 4, "certainties should be distinguishable");
    }

    #[test]
    fn an_unavailable_figure_still_renders_as_a_dash_in_the_tui() {
        let m: Measured<u64> =
            Measured::unavailable(aum_contract::UnavailableReason::NotReportedByProvider {
                field: "reasoning".to_owned(),
                detail: "not reported".to_owned(),
            });
        assert_eq!(tokens_span(&m).content, "—");
    }
}
