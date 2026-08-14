//! Rendering numbers to text without overstating them.
//!
//! The desktop interface carried certainty on five channels. A terminal has
//! fewer, and colour is the one that survives least — piped, redirected, or
//! read by someone who cannot distinguish the hues, it is simply gone. So the
//! prefix does the work and colour only reinforces it:
//!
//! | prefix | meaning |
//! |---|---|
//! | *(none)* | exact — the provider's own number |
//! | `≈` | calculated — we derived it, the provider did not send it |
//! | `≥` | partial — a floor, because some contributor was unmeasured |
//! | `—` | unavailable — and never `0`, which is a different claim |
//!
//! `render_tokens` and `render_money` are the only ways a figure reaches the
//! screen, so there is no path by which a missing measurement becomes a
//! confident zero.

use aum_contract::{DisplayKind, Measured, Money};

/// Thousands separators, because a nine-digit token count is unreadable without.
#[must_use]
pub fn thousands(n: i64) -> String {
    let neg = n < 0;
    let digits = n.abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if neg { format!("-{out}") } else { out }
}

#[must_use]
pub const fn prefix(kind: DisplayKind) -> &'static str {
    match kind {
        DisplayKind::Exact => "",
        DisplayKind::Calculated | DisplayKind::Estimated => "≈",
        DisplayKind::Partial => "≥",
        DisplayKind::Unavailable => "",
    }
}

/// An ANSI colour for a certainty, or nothing when colour is off.
#[must_use]
pub const fn colour(kind: DisplayKind) -> &'static str {
    match kind {
        DisplayKind::Exact => "\x1b[38;5;114m",
        DisplayKind::Calculated => "\x1b[38;5;110m",
        DisplayKind::Estimated | DisplayKind::Partial => "\x1b[38;5;179m",
        DisplayKind::Unavailable => "\x1b[38;5;244m",
    }
}

pub const RESET: &str = "\x1b[0m";
pub const DIM: &str = "\x1b[2m";
pub const BOLD: &str = "\x1b[1m";

/// A token count.
#[must_use]
pub fn render_tokens(m: &Measured<u64>, colourise: bool) -> String {
    let kind = m.accuracy.display_kind();
    let body = match m.value {
        // An em dash, never a zero. "We did not measure this" and "this was
        // zero" are different facts and the second one is a claim.
        None => "—".to_owned(),
        Some(v) => format!(
            "{}{}",
            prefix(kind),
            thousands(i64::try_from(v).unwrap_or(i64::MAX))
        ),
    };
    paint(&body, kind, colourise)
}

/// A monetary amount.
///
/// Small amounts keep more digits: a per-request cost is routinely a fraction
/// of a cent, and two decimal places would render every one of them as 0.00.
#[must_use]
pub fn render_money(m: &Measured<Money>, currency: &str, colourise: bool) -> String {
    let kind = m.accuracy.display_kind();
    let body = match &m.value {
        None => "—".to_owned(),
        Some(v) => {
            let amount = v.amount();
            let abs = amount.abs();
            // Below a cent, six places — a per-request cost is routinely
            // $0.004125, and four places would quietly drop the last two
            // digits of a figure whose whole value is in them.
            let digits = if abs >= rust_decimal::Decimal::ONE {
                2
            } else if abs >= rust_decimal::Decimal::new(1, 2) {
                4
            } else {
                6
            };
            format!(
                "{}{}{}",
                prefix(kind),
                symbol(currency),
                round_to(amount, digits)
            )
        }
    };
    paint(&body, kind, colourise)
}

fn round_to(d: rust_decimal::Decimal, places: u32) -> String {
    let rounded = d.round_dp(places);
    let s = rounded.to_string();
    // `to_string` drops trailing zeros; money reads better with them kept.
    match s.split_once('.') {
        Some((whole, frac)) => format!("{whole}.{:0<width$}", frac, width = places as usize),
        None => format!("{s}.{}", "0".repeat(places as usize)),
    }
}

#[must_use]
pub fn symbol(currency: &str) -> &'static str {
    match currency {
        "EUR" => "€",
        "CZK" => "Kč",
        _ => "$",
    }
}

fn paint(body: &str, kind: DisplayKind, colourise: bool) -> String {
    if colourise {
        format!("{}{body}{RESET}", colour(kind))
    } else {
        body.to_owned()
    }
}

/// The one-line legend that makes the prefixes readable without documentation.
#[must_use]
pub fn legend(colourise: bool) -> String {
    let body = "exact · ≈ calculated · ≥ at least · — not measured";
    if colourise {
        format!("{DIM}{body}{RESET}")
    } else {
        body.to_owned()
    }
}

// ── Tables ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// A plain text table.
///
/// Hand-rolled rather than pulled from a crate: the output is meant to be
/// grep-able and paste-able, so it is columns and spaces with no box drawing,
/// and the whole thing is forty lines.
pub struct Table {
    headers: Vec<String>,
    aligns: Vec<Align>,
    rows: Vec<Vec<String>>,
}

impl Table {
    #[must_use]
    pub fn new(headers: &[(&str, Align)]) -> Self {
        Self {
            headers: headers.iter().map(|(h, _)| (*h).to_owned()).collect(),
            aligns: headers.iter().map(|(_, a)| *a).collect(),
            rows: Vec::new(),
        }
    }

    pub fn push(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }

    /// Render, padding to the widest cell in each column.
    ///
    /// Width is counted in characters rather than bytes, because the certainty
    /// prefixes and the currency symbols are multi-byte and a byte count would
    /// misalign every column that contains one.
    #[must_use]
    pub fn render(&self, colourise: bool) -> String {
        let mut widths: Vec<usize> = self.headers.iter().map(|h| visible_width(h)).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                if let Some(w) = widths.get_mut(i) {
                    *w = (*w).max(visible_width(cell));
                }
            }
        }

        let mut out = String::new();
        let header: Vec<String> = self
            .headers
            .iter()
            .enumerate()
            .map(|(i, h)| pad(h, widths[i], self.aligns[i]))
            .collect();
        if colourise {
            out.push_str(&format!("{BOLD}{}{RESET}\n", header.join("  ").trim_end()));
        } else {
            out.push_str(header.join("  ").trim_end());
            out.push('\n');
        }

        for row in &self.rows {
            let cells: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, c)| pad(c, widths.get(i).copied().unwrap_or(0), self.aligns[i]))
                .collect();
            out.push_str(cells.join("  ").trim_end());
            out.push('\n');
        }
        out
    }
}

/// Printable width, ignoring ANSI escape sequences.
fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    for c in s.chars() {
        if in_escape {
            if c == 'm' {
                in_escape = false;
            }
        } else if c == '\x1b' {
            in_escape = true;
        } else {
            width += 1;
        }
    }
    width
}

fn pad(s: &str, width: usize, align: Align) -> String {
    let visible = visible_width(s);
    let fill = width.saturating_sub(visible);
    match align {
        Align::Left => format!("{s}{}", " ".repeat(fill)),
        Align::Right => format!("{}{s}", " ".repeat(fill)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use aum_contract::{MeasurementSource, UnavailableReason};

    #[test]
    fn counts_are_grouped_for_reading() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(2_293_396_738), "2,293,396,738");
        assert_eq!(thousands(-1_500), "-1,500");
    }

    #[test]
    fn an_unavailable_count_is_a_dash_and_never_a_zero() {
        // The single highest-impact rendering bug available in this product.
        let m: Measured<u64> = Measured::unavailable(UnavailableReason::NotReportedByProvider {
            field: "reasoning".to_owned(),
            detail: "Claude Code does not report reasoning tokens.".to_owned(),
        });
        assert_eq!(render_tokens(&m, false), "—");
    }

    #[test]
    fn a_real_zero_still_renders_as_zero() {
        let m = Measured::exact(0_u64, MeasurementSource::ProviderReported);
        assert_eq!(render_tokens(&m, false), "0");
    }

    #[test]
    fn certainty_survives_without_colour() {
        // Piped into a file, colour is gone. The prefix must carry it alone.
        let exact = Measured::exact(1_234_u64, MeasurementSource::ProviderReported);
        let calc = Measured::calculated(1_234_u64, MeasurementSource::ApplicationTelemetry);
        let part = Measured::partial(1_234_u64, 2, 5, "2 of 5 reported".to_owned());

        assert_eq!(render_tokens(&exact, false), "1,234");
        assert_eq!(render_tokens(&calc, false), "≈1,234");
        assert_eq!(render_tokens(&part, false), "≥1,234");
    }

    #[test]
    fn an_unpriced_model_shows_a_dash_and_not_a_free_lunch() {
        let m: Measured<Money> = Measured::unavailable(UnavailableReason::NoPricingForModel {
            model_id: "gpt-5.6-sol".to_owned(),
        });
        let rendered = render_money(&m, "USD", false);
        assert_eq!(rendered, "—");
        assert!(!rendered.contains('0'), "must not read as zero: {rendered}");
    }

    #[test]
    fn a_fraction_of_a_cent_keeps_enough_digits_to_exist() {
        // Two decimal places would render every per-request cost as $0.00.
        let m = Measured::calculated(
            Money::new(rust_decimal::Decimal::new(4125, 6)),
            MeasurementSource::ApplicationTelemetry,
        );
        assert_eq!(render_money(&m, "USD", false), "≈$0.004125");
    }

    #[test]
    fn a_normal_amount_keeps_its_trailing_zeros() {
        let m = Measured::calculated(
            Money::new(rust_decimal::Decimal::new(9000, 2)),
            MeasurementSource::ApplicationTelemetry,
        );
        assert_eq!(render_money(&m, "USD", false), "≈$90.00");
    }

    #[test]
    fn the_currency_symbol_follows_the_currency() {
        let m = Measured::calculated(
            Money::new(rust_decimal::Decimal::new(8280, 2)),
            MeasurementSource::ApplicationTelemetry,
        );
        assert_eq!(render_money(&m, "EUR", false), "≈€82.80");
        assert_eq!(render_money(&m, "CZK", false), "≈Kč82.80");
    }

    #[test]
    fn columns_align_even_when_cells_contain_multibyte_prefixes() {
        // `≈` and `€` are three bytes each. Padding by byte length would push
        // every row containing one out of line.
        let mut t = Table::new(&[("model", Align::Left), ("cost", Align::Right)]);
        t.push(vec!["a".to_owned(), "≈€1.00".to_owned()]);
        t.push(vec!["bbbb".to_owned(), "—".to_owned()]);
        let out = t.render(false);

        let widths: Vec<usize> = out.lines().map(|l| l.chars().count()).collect();
        let first = out.lines().nth(1).unwrap();
        let second = out.lines().nth(2).unwrap();
        assert_eq!(
            first.chars().count(),
            second.chars().count(),
            "rows should align: {widths:?}\n{out}"
        );
    }

    #[test]
    fn colour_never_changes_the_visible_width() {
        let coloured = render_tokens(
            &Measured::exact(1_234_u64, MeasurementSource::ProviderReported),
            true,
        );
        assert_eq!(visible_width(&coloured), 5, "1,234 is five characters");
        assert!(coloured.contains('\x1b'), "should actually be coloured");
    }
}
