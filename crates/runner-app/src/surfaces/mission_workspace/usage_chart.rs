//! What the usage card draws, worked out from a mission's calls and kept apart
//! from the drawing so it can be tested: the headline figure, the meter across
//! the week's allowance, and the calls grouped by slot with the largest flagged.
//!
//! A bar is a share of the mission's total in the chosen unit: points of the
//! allowance, or tokens. Points are a range (cached input counting against the
//! allowance or not), so bars use its middle and the text gives both ends.

use runner_core::protocol::CallUsage;
use std::collections::BTreeMap;

/// Calls listed per slot when it is open; the rest are summed in one row.
pub(super) const CALLS_SHOWN: usize = 8;
/// Narrowest a bar may be drawn, as a fraction, so a small share stays visible.
pub(super) const MIN_BAR: f32 = 0.012;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum UsageMode {
    #[default]
    Points,
    Tokens,
}

/// A figure in points: both ends of the range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Range {
    pub low: f64,
    pub high: f64,
}

impl Range {
    pub fn mid(self) -> f64 {
        (self.low + self.high) / 2.0
    }
}

/// `0.05`, `1.4` or `12`: two decimals under a tenth, one under ten, none above.
pub(crate) fn points_text(value: f64) -> String {
    if value >= 10.0 {
        format!("{value:.0}")
    } else if value >= 0.1 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

/// The headline for a range of points: `under 0.1`, `1.4`, `1.4 to 2.1`.
pub(super) enum Headline {
    Under(String),
    /// A range whose low end rounds to nothing: `up to 0.02`.
    UpTo(String),
    One(String),
    Between(String, String),
}

pub(super) fn headline(range: Range) -> Headline {
    if range.high < 0.01 {
        return Headline::Under("0.01".to_owned());
    }
    if range.low < 0.005 {
        return Headline::UpTo(points_text(range.high));
    }
    // Both ends with the same number of decimals: `0.07 to 0.10`, never `0.07 to 0.1`.
    let decimals = if range.high >= 10.0 {
        0
    } else if range.low >= 0.1 {
        1
    } else {
        2
    };
    let (low, high) = (format!("{:.decimals$}", range.low), format!("{:.decimals$}", range.high));
    if low == high {
        Headline::One(high)
    } else {
        Headline::Between(low, high)
    }
}

/// How the week's allowance divides, as fractions of the whole (0 to 1): what
/// others used, what this mission used (solid up to its low end, lighter to its
/// high end) and what is left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Meter {
    pub others: f32,
    pub mission_solid: f32,
    pub mission_range: f32,
    pub left: f32,
}

pub(super) fn meter(window_points: f64, mission: Range) -> Meter {
    let used = (window_points / 100.0).clamp(0.0, 1.0) as f32;
    let high = (mission.high / 100.0).clamp(0.0, 1.0) as f32;
    let low = (mission.low / 100.0).clamp(0.0, 1.0) as f32;
    let mission_total = high.min(used).max(0.0);
    let solid = low.min(mission_total);
    Meter {
        others: (used - mission_total).max(0.0),
        mission_solid: solid,
        mission_range: (mission_total - solid).max(0.0),
        left: (1.0 - used).max(0.0),
    }
}

/// One model call as a row.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CallRow {
    pub usage_id: String,
    /// Local time of day the call was recorded, `HH:MM:SS`.
    pub time: String,
    pub points: Option<Range>,
    pub tokens: i64,
    pub input: i64,
    pub cached: i64,
    pub output: i64,
    /// This call's share of the mission's total in the chosen unit, 0 to 1.
    pub share: f32,
    pub largest: bool,
}

/// One slot's calls: its totals, its biggest calls in time order, and the rest.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SlotGroup {
    pub handle: String,
    pub runtime: String,
    pub points: Option<Range>,
    pub tokens: i64,
    pub share: f32,
    pub calls: usize,
    pub shown: Vec<CallRow>,
    pub rest_calls: usize,
    pub rest_points: Option<Range>,
    pub rest_tokens: i64,
    pub rest_share: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Chart {
    pub groups: Vec<SlotGroup>,
    /// The costliest call: its id and the handle of its slot.
    pub largest: Option<(String, String)>,
    /// Whether every call has points. When not, the chart can only show tokens.
    pub has_points: bool,
    /// The middle of the mission's range in points, when every call has points.
    /// Slots show their share of this, so they add up to the headline.
    pub points_mid: Option<f64>,
}

/// Input and output, cached input counted in full: the measure bars use in tokens mode.
fn token_total(call: &CallUsage) -> i64 {
    call.input_tokens + call.output_tokens
}

/// The two readings of a figure in points: with cached input counted in
/// full, and counted as free. They are kept apart while adding up, and only
/// then turned into a range, so the parts of a total always agree with it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Pair {
    in_full: f64,
    free: f64,
}

impl Pair {
    fn range(self) -> Range {
        Range {
            low: self.in_full.min(self.free),
            high: self.in_full.max(self.free),
        }
    }
}

fn pair_of(call: &CallUsage) -> Option<Pair> {
    Some(Pair {
        in_full: call.points_cached_in_full?,
        free: call.points_cached_free?,
    })
}

fn range_of(call: &CallUsage) -> Option<Range> {
    pair_of(call).map(Pair::range)
}

fn add_pair(total: Option<Pair>, next: Option<Pair>) -> Option<Pair> {
    let (a, b) = (total?, next?);
    Some(Pair {
        in_full: a.in_full + b.in_full,
        free: a.free + b.free,
    })
}

fn local_time(at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|time| time.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Groups calls by slot. `handle_of` names the slot of a session key; calls
/// whose session is unknown are grouped as `?`.
pub(super) fn build_chart(
    calls: &[CallUsage],
    mode: UsageMode,
    handle_of: &dyn Fn(&str) -> Option<String>,
) -> Chart {
    let has_points = !calls.is_empty() && calls.iter().all(|call| range_of(call).is_some());
    let mode = if has_points { mode } else { UsageMode::Tokens };
    let value = |call: &CallUsage| -> f64 {
        match mode {
            UsageMode::Points => range_of(call).map_or(0.0, Range::mid),
            UsageMode::Tokens => token_total(call) as f64,
        }
    };
    let total: f64 = calls.iter().map(value).sum();
    let share = |amount: f64| -> f32 {
        if total > 0.0 {
            (amount / total) as f32
        } else {
            0.0
        }
    };
    let largest = calls
        .iter()
        .max_by(|a, b| value(a).total_cmp(&value(b)))
        .filter(|call| value(call) > 0.0)
        .map(|call| call.usage_id.clone());

    let mut by_slot: BTreeMap<String, (String, Vec<&CallUsage>)> = BTreeMap::new();
    for call in calls {
        let handle = handle_of(&call.session_key).unwrap_or_else(|| "?".to_owned());
        by_slot
            .entry(handle)
            .or_insert_with(|| (call.runtime.clone(), Vec::new()))
            .1
            .push(call);
    }
    let mut groups: Vec<SlotGroup> = by_slot
        .into_iter()
        .map(|(handle, (runtime, mut members))| {
            members.sort_by(|a, b| (&a.at, &a.usage_id).cmp(&(&b.at, &b.usage_id)));
            let group_value: f64 = members.iter().map(|call| value(call)).sum();
            let points = members
                .iter()
                .fold(Some(Pair::default()), |sum, call| add_pair(sum, pair_of(call)))
                .map(Pair::range);
            let tokens: i64 = members.iter().map(|call| token_total(call)).sum();
            // The biggest calls, shown in the order they happened.
            let mut ranked: Vec<&&CallUsage> = members.iter().collect();
            ranked.sort_by(|a, b| value(b).total_cmp(&value(a)));
            let keep: std::collections::HashSet<&str> = ranked
                .iter()
                .take(CALLS_SHOWN)
                .map(|call| call.usage_id.as_str())
                .collect();
            let (shown_calls, rest): (Vec<&CallUsage>, Vec<&CallUsage>) = members
                .iter()
                .copied()
                .partition(|call| keep.contains(call.usage_id.as_str()));
            let shown = shown_calls
                .iter()
                .map(|call| CallRow {
                    usage_id: call.usage_id.clone(),
                    time: local_time(&call.at),
                    points: range_of(call),
                    tokens: token_total(call),
                    input: call.input_tokens,
                    cached: call.cached_tokens,
                    output: call.output_tokens,
                    share: share(value(call)),
                    largest: largest.as_deref() == Some(call.usage_id.as_str()),
                })
                .collect();
            let rest_value: f64 = rest.iter().map(|call| value(call)).sum();
            SlotGroup {
                handle,
                runtime,
                points,
                tokens,
                share: share(group_value),
                calls: members.len(),
                shown,
                rest_calls: rest.len(),
                rest_points: rest
                    .iter()
                    .fold(Some(Pair::default()), |sum, call| add_pair(sum, pair_of(call)))
                    .map(Pair::range),
                rest_tokens: rest.iter().map(|call| token_total(call)).sum(),
                rest_share: share(rest_value),
            }
        })
        .collect();
    // The slot that used most first.
    groups.sort_by(|a, b| b.share.total_cmp(&a.share).then(a.handle.cmp(&b.handle)));
    let largest = largest.and_then(|id| {
        groups
            .iter()
            .find(|group| group.shown.iter().any(|row| row.usage_id == id))
            .map(|group| (id, group.handle.clone()))
    });
    let points_mid = has_points.then(|| calls.iter().filter_map(range_of).map(Range::mid).sum());
    Chart {
        groups,
        largest,
        has_points,
        points_mid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, key: &str, at: &str, input: i64, points: Option<(f64, f64)>) -> CallUsage {
        CallUsage {
            usage_id: id.into(),
            runtime: "muse".into(),
            session_key: key.into(),
            at: at.into(),
            input_tokens: input,
            output_tokens: 0,
            cached_tokens: 0,
            reasoning_tokens: 0,
            points_cached_in_full: points.map(|p| p.0),
            points_cached_free: points.map(|p| p.1),
        }
    }

    fn names(key: &str) -> Option<String> {
        match key {
            "k1" => Some("coder".into()),
            "k2" => Some("reviewer".into()),
            _ => None,
        }
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn the_headline_says_under_a_tenth_one_figure_or_a_range() {
        assert!(matches!(headline(Range { low: 0.002, high: 0.008 }), Headline::Under(t) if t == "0.01"));
        assert!(matches!(headline(Range { low: 0.009, high: 0.039 }), Headline::Between(a, b) if (a.as_str(), b.as_str()) == ("0.01", "0.04")));
        assert_eq!(points_text(0.05), "0.05");
        assert_eq!(points_text(0.4), "0.4");
        assert_eq!(points_text(9.96), "10.0", "rounds up, never shows 10.0 as 9.9");
        assert!(matches!(headline(Range { low: 1.42, high: 1.44 }), Headline::One(t) if t == "1.4"));
        assert!(matches!(headline(Range { low: 0.07, high: 0.10 }), Headline::Between(a, b) if (a.as_str(), b.as_str()) == ("0.07", "0.10")));
        assert!(matches!(headline(Range { low: 0.001, high: 0.02 }), Headline::UpTo(t) if t == "0.02"));
        assert!(matches!(headline(Range { low: 1.4, high: 2.1 }), Headline::Between(a, b) if (a.as_str(), b.as_str()) == ("1.4", "2.1")));
        assert!(matches!(headline(Range { low: 12.2, high: 12.4 }), Headline::One(t) if t == "12"));
    }

    #[test]
    fn the_meter_splits_the_week_into_others_this_mission_and_what_is_left() {
        let m = meter(44.0, Range { low: 2.0, high: 6.0 });
        assert!(near(m.others, 0.38) && near(m.mission_solid, 0.02) && near(m.mission_range, 0.04) && near(m.left, 0.56));
        assert!(near(m.others + m.mission_solid + m.mission_range + m.left, 1.0));
        // A mission cannot have used more than the window shows, and a full window has nothing left.
        let over = meter(10.0, Range { low: 5.0, high: 30.0 });
        assert!(near(over.mission_solid + over.mission_range, 0.10) && near(over.others, 0.0));
        let full = meter(100.0, Range { low: 1.0, high: 2.0 });
        assert!(near(full.left, 0.0));
    }

    #[test]
    fn calls_group_by_slot_with_shares_of_the_mission_total() {
        let calls = [
            call("a", "k1", "2026-11-12T10:00:00Z", 100, Some((1.0, 1.0))),
            call("b", "k1", "2026-11-12T10:05:00Z", 300, Some((3.0, 3.0))),
            call("c", "k2", "2026-11-12T10:06:00Z", 100, Some((1.0, 1.0))),
        ];
        let chart = build_chart(&calls, UsageMode::Points, &names);
        assert!(chart.has_points);
        assert!(chart.points_mid.is_some_and(|mid| (mid - 5.0).abs() < 1e-9));
        assert_eq!(chart.groups.iter().map(|g| g.handle.as_str()).collect::<Vec<_>>(), ["coder", "reviewer"]);
        assert!(near(chart.groups[0].share, 0.8) && near(chart.groups[1].share, 0.2));
        assert_eq!(chart.groups[0].points, Some(Range { low: 4.0, high: 4.0 }));
        assert_eq!(chart.largest, Some(("b".to_owned(), "coder".to_owned())));
        assert!(chart.groups[0].shown.iter().any(|row| row.largest && row.usage_id == "b"));
        assert!(near(chart.groups[0].shown[1].share, 0.6));
    }

    #[test]
    fn only_the_biggest_calls_are_listed_and_the_rest_are_summed_in_time_order() {
        let mut calls = Vec::new();
        for i in 0..12 {
            // Calls 0..11 with growing size; the four smallest are 0..3.
            calls.push(call(&format!("c{i:02}"), "k1", &format!("2026-11-12T10:{i:02}:00Z"), (i + 1) * 10, Some((i as f64, i as f64))));
        }
        let chart = build_chart(&calls, UsageMode::Tokens, &names);
        let group = &chart.groups[0];
        assert_eq!(group.calls, 12);
        assert_eq!(group.shown.len(), CALLS_SHOWN);
        assert_eq!(group.rest_calls, 4);
        assert_eq!(group.rest_tokens, 10 + 20 + 30 + 40);
        let order: Vec<&str> = group.shown.iter().map(|row| row.usage_id.as_str()).collect();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted, "shown calls keep the order they happened in");
        assert!(near(group.shown.iter().map(|r| r.share).sum::<f32>() + group.rest_share, 1.0));
    }

    #[test]
    fn calls_without_points_fall_back_to_tokens_and_say_so() {
        let calls = [call("a", "k1", "2026-11-12T10:00:00Z", 100, None), call("b", "k1", "2026-11-12T10:01:00Z", 300, Some((1.0, 2.0)))];
        let chart = build_chart(&calls, UsageMode::Points, &names);
        assert!(!chart.has_points, "one call without points makes the whole chart tokens-only");
        assert!(near(chart.groups[0].shown[1].share, 0.75), "bars are token shares then");
    }

    #[test]
    fn unknown_sessions_are_grouped_apart_and_an_empty_mission_has_no_chart() {
        let calls = [call("a", "unknown-key", "2026-11-12T10:00:00Z", 5, Some((0.1, 0.1)))];
        assert_eq!(build_chart(&calls, UsageMode::Points, &names).groups[0].handle, "?");
        let none = build_chart(&[], UsageMode::Points, &names);
        assert!(none.groups.is_empty() && none.largest.is_none() && !none.has_points);
    }
}
