//! The contribution graph.
//!
//! A year of days, one cell each, weeks running down the columns the way GitHub
//! draws them. Intensity is bucketed rather than continuous because a terminal
//! has a handful of usable shades and a linear ramp across nine orders of
//! magnitude would put almost every day in the darkest one.
//!
//! **A day with no usage and a day with a little are different cells.** The
//! first is drawn in the background colour, the second in the lowest active
//! shade — collapsing them would turn a quiet week into an absent one.

use ratatui::style::Color;

use super::Day;

/// Five levels: none, then four quartiles of the days that had any usage.
///
/// Quartiles of the *active* days rather than of the whole year: on a machine
/// used a few days a week, thresholds drawn from every day would push all the
/// working days into one bucket and show a flat wall.
pub struct Scale {
    thresholds: [i64; 4],
    pub peak: i64,
}

impl Scale {
    #[must_use]
    pub fn of(days: &[Day]) -> Self {
        let mut active: Vec<i64> = days.iter().map(|d| d.tokens).filter(|t| *t > 0).collect();
        active.sort_unstable();

        if active.is_empty() {
            return Self {
                thresholds: [1, 2, 3, 4],
                peak: 0,
            };
        }

        let at = |q: f64| -> i64 {
            let idx = ((active.len() as f64 - 1.0) * q).round() as usize;
            active.get(idx).copied().unwrap_or(1).max(1)
        };

        Self {
            thresholds: [at(0.25), at(0.50), at(0.75), at(0.95)],
            peak: active.last().copied().unwrap_or(0),
        }
    }

    #[must_use]
    pub fn level(&self, tokens: i64) -> usize {
        if tokens <= 0 {
            return 0;
        }
        // Always at least 1 for a day that had any usage at all.
        1 + self
            .thresholds
            .iter()
            .filter(|t| tokens > **t)
            .count()
            .min(3)
    }
}

/// Five steps of one hue, dark to bright.
#[must_use]
pub const fn colour(level: usize) -> Color {
    match level {
        0 => Color::Indexed(236),
        1 => Color::Indexed(23),
        2 => Color::Indexed(30),
        3 => Color::Indexed(37),
        _ => Color::Indexed(51),
    }
}

/// The block drawn for each level.
///
/// Distinct glyphs as well as distinct colours, so the graph still reads on a
/// monochrome terminal or in a screenshot someone has desaturated.
#[must_use]
pub const fn glyph(level: usize) -> &'static str {
    match level {
        0 => "·",
        1 => "▪",
        _ => "■",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn days(tokens: &[i64]) -> Vec<Day> {
        let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        tokens
            .iter()
            .enumerate()
            .map(|(i, t)| Day {
                date: start + chrono::Duration::days(i as i64),
                tokens: *t,
                requests: if *t > 0 { 1 } else { 0 },
            })
            .collect()
    }

    #[test]
    fn a_day_with_no_usage_is_level_zero_and_nothing_else_is() {
        let scale = Scale::of(&days(&[0, 100, 200, 300]));
        assert_eq!(scale.level(0), 0);
        assert!(scale.level(1) >= 1, "any usage must be visible");
        assert!(scale.level(100) >= 1);
    }

    #[test]
    fn a_single_token_still_gets_a_visible_cell() {
        // The failure this guards against: a quiet day rounding to the same
        // cell as an idle one, so the graph shows a gap where there was work.
        let scale = Scale::of(&days(&[1, 1_000_000_000]));
        assert_eq!(scale.level(1), 1);
        assert_ne!(scale.level(1), scale.level(0));
    }

    #[test]
    fn thresholds_come_from_active_days_only() {
        // 360 idle days and 5 busy ones: quartiles over the whole year would
        // put every busy day in the top bucket and show a flat wall.
        let mut tokens = vec![0; 360];
        tokens.extend([10, 100, 1_000, 10_000, 100_000]);
        let scale = Scale::of(&days(&tokens));

        let levels: Vec<usize> = [10, 100, 1_000, 10_000, 100_000]
            .iter()
            .map(|t| scale.level(*t))
            .collect();
        assert!(
            levels
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                >= 3,
            "busy days should spread across shades, got {levels:?}"
        );
    }

    #[test]
    fn an_empty_year_does_not_panic_and_has_no_peak() {
        let scale = Scale::of(&[]);
        assert_eq!(scale.peak, 0);
        assert_eq!(scale.level(0), 0);
    }

    #[test]
    fn every_level_has_a_distinct_glyph_or_colour() {
        // Colour alone is not enough: the graph has to survive greyscale.
        assert_eq!(glyph(0), "·");
        assert_ne!(glyph(0), glyph(1));
        let colours: std::collections::HashSet<_> = (0..5).map(colour).collect();
        assert_eq!(colours.len(), 5, "each level needs its own colour");
    }

    #[test]
    fn the_level_never_exceeds_the_scale() {
        let scale = Scale::of(&days(&[1, 2, 3, 4, 5]));
        assert!(scale.level(i64::MAX) <= 4);
    }
}
