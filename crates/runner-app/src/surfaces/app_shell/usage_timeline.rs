//! What the Usage page shows, worked out from the daemon's list of recent
//! missions and kept apart from the drawing so it can be tested: a filter, a
//! summary, daily bars, and one lane per mission on a clock shared with the
//! bars. A mission that used a lot of the week and went badly, or was never
//! rated, is flagged: a mistake is easier to forgive when the run was cheap.
//!
//! Points are of the weekly allowance, split by tokens by the daemon as a range
//! (cached input may or may not count against it); the middle of the range is
//! used for ranking and sizing, and the range is shown beside it.

use chrono::{DateTime, Duration, Utc};
use runner_core::protocol::{MissionUsageSummary, RatingVerdict};

/// Narrowest a lane's bar is drawn, as a fraction of the axis, so a short
/// mission stays visible on a long one.
pub(super) const MIN_BAR: f32 = 0.006;
/// The "expensive" lines the page offers, in points of the weekly allowance.
pub(super) const EXPENSIVE_CHOICES: [f64; 4] = [1.0, 2.0, 5.0, 10.0];
/// The default line. A guess to be tuned, not a measurement: set so that the
/// dearest few of a typical week's crew missions fall above it.
pub(super) const DEFAULT_EXPENSIVE: f64 = 2.0;
/// Days the page can look back.
pub(super) const DAY_CHOICES: [u32; 3] = [7, 14, 30];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Filter {
    #[default]
    All,
    Expensive,
    WentBadly,
    NotRated,
}

pub(super) const FILTERS: [Filter; 4] = [Filter::All, Filter::Expensive, Filter::WentBadly, Filter::NotRated];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Flag {
    None,
    /// Expensive and rated down.
    WentBadly,
    /// Expensive and not rated yet.
    NotRated,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Lane {
    pub mission_id: String,
    pub title: String,
    pub runtime: Option<String>,
    pub low: f64,
    pub high: f64,
    pub mid: f64,
    pub verdict: Option<RatingVerdict>,
    pub started_at: DateTime<Utc>,
    pub flag: Flag,
    /// Where the bar starts and how wide it is, as fractions of the axis:
    /// when the mission ran, which says nothing about how much it used.
    pub left: f32,
    pub width: f32,
    /// How much it used, as fractions of the largest mission's high figure:
    /// solid up to the low end of its range, lighter up to the high end.
    pub solid: f32,
    pub reach: f32,
}

/// One mission on the cumulative curve: how much of the period's total had
/// been used once it finished, with the mission's flag for its dot.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CurvePoint {
    pub x: f32,
    pub y: f32,
    pub flag: Flag,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Summary {
    pub points: f64,
    pub measured: usize,
    pub flagged: usize,
    pub unmeasured: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Counts {
    pub all: usize,
    pub expensive: usize,
    pub went_badly: usize,
    pub not_rated: usize,
}

impl Counts {
    pub fn of(self, filter: Filter) -> usize {
        match filter {
            Filter::All => self.all,
            Filter::Expensive => self.expensive,
            Filter::WentBadly => self.went_badly,
            Filter::NotRated => self.not_rated,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Timeline {
    pub lanes: Vec<Lane>,
    /// The cumulative allowance used over the period, oldest first: `y` is a
    /// fraction of the total and reaches 1.0 at the last mission.
    pub curve: Vec<CurvePoint>,
    /// A label for each day of the axis, oldest first.
    pub day_labels: Vec<String>,
    pub summary: Summary,
    pub counts: Counts,
    /// Day boundaries on the axis, as fractions, for the grid.
    pub grid: Vec<f32>,
    /// The flagged lanes, dearest first, for the "needs a look" queue.
    pub queue: Vec<Lane>,
    /// The largest high figure among the lanes, which the points bars are scaled to.
    pub scale: f64,
}

fn parse(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn matches(lane: &Lane, filter: Filter, expensive_at: f64) -> bool {
    match filter {
        Filter::All => true,
        Filter::Expensive => lane.mid >= expensive_at,
        Filter::WentBadly => lane.verdict == Some(RatingVerdict::Down),
        Filter::NotRated => lane.verdict.is_none(),
    }
}

/// Builds the page's model for the last `days` days ending at `now`.
pub(super) fn build(
    list: &[MissionUsageSummary],
    days: u32,
    now: DateTime<Utc>,
    expensive_at: f64,
    filter: Filter,
) -> Timeline {
    let days = days.max(1);
    let span = Duration::days(i64::from(days));
    let axis_start = now - span;
    let axis_ms = span.num_milliseconds().max(1) as f64;
    let frac = |time: DateTime<Utc>| -> f32 {
        (((time - axis_start).num_milliseconds() as f64) / axis_ms).clamp(0.0, 1.0) as f32
    };

    let mut lanes: Vec<Lane> = list
        .iter()
        .filter_map(|mission| {
            let (low, high) = (mission.attributed_low?, mission.attributed_high?);
            let started_at = parse(&mission.started_at)?;
            // A stopped mission ends when it was stopped. One never stopped ends
            // at its last model call, the latest sign it did anything; the time
            // its allowance was last read says when someone looked, not when it ran.
            let ended = mission
                .stopped_at
                .as_deref()
                .and_then(parse)
                .or_else(|| mission.last_call_at.as_deref().and_then(parse))
                .or_else(|| mission.last_read_at.as_deref().and_then(parse))
                .filter(|end| *end >= started_at)
                .unwrap_or(started_at);
            let start = frac(started_at);
            let width = (frac(ended) - start).max(MIN_BAR);
            let mid = (low + high) / 2.0;
            let expensive = mid >= expensive_at;
            Some(Lane {
                mission_id: mission.mission_id.clone(),
                title: mission.title.clone(),
                runtime: mission.runtime.clone(),
                low,
                high,
                mid,
                verdict: mission.verdict,
                started_at,
                flag: match (expensive, mission.verdict) {
                    (true, Some(RatingVerdict::Down)) => Flag::WentBadly,
                    (true, None) => Flag::NotRated,
                    _ => Flag::None,
                },
                left: start.min(1.0 - width).max(0.0),
                width,
                solid: 0.0,
                reach: 0.0,
            })
        })
        .collect();
    let unmeasured = list.len() - lanes.len();
    let counts = Counts {
        all: lanes.len(),
        expensive: lanes.iter().filter(|lane| matches(lane, Filter::Expensive, expensive_at)).count(),
        went_badly: lanes.iter().filter(|lane| matches(lane, Filter::WentBadly, expensive_at)).count(),
        not_rated: lanes.iter().filter(|lane| matches(lane, Filter::NotRated, expensive_at)).count(),
    };
    lanes.retain(|lane| matches(lane, filter, expensive_at));
    // Newest first, like a log.
    lanes.sort_by(|a, b| b.started_at.cmp(&a.started_at).then_with(|| a.mission_id.cmp(&b.mission_id)));

    let scale = lanes.iter().map(|lane| lane.high).fold(0.0_f64, f64::max);
    for lane in &mut lanes {
        if scale > 0.0 {
            lane.solid = (lane.low / scale) as f32;
            lane.reach = (lane.high / scale) as f32;
        }
    }

    // The curve: missions in the order they finished, each adding its share.
    let total: f64 = lanes.iter().map(|lane| lane.mid).sum();
    let mut by_end: Vec<&Lane> = lanes.iter().collect();
    by_end.sort_by(|a, b| (a.left + a.width).total_cmp(&(b.left + b.width)));
    let mut running = 0.0;
    let curve: Vec<CurvePoint> = by_end
        .iter()
        .map(|lane| {
            running += lane.mid;
            CurvePoint {
                x: (lane.left + lane.width).min(1.0),
                y: if total > 0.0 { (running / total) as f32 } else { 0.0 },
                flag: lane.flag,
            }
        })
        .collect();
    let day_labels: Vec<String> = (0..days)
        .map(|index| {
            let middle = axis_start + Duration::days(i64::from(index)) + Duration::hours(12);
            middle.with_timezone(&chrono::Local).format("%a %-d").to_string()
        })
        .collect();
    let grid = (0..=days).map(|index| index as f32 / days as f32).collect();

    let mut queue: Vec<Lane> = lanes.iter().filter(|lane| lane.flag != Flag::None).cloned().collect();
    queue.sort_by(|a, b| b.mid.total_cmp(&a.mid));

    Timeline {
        summary: Summary {
            points: lanes.iter().map(|lane| lane.mid).sum(),
            measured: lanes.len(),
            flagged: queue.len(),
            unmeasured,
        },
        counts,
        curve,
        day_labels,
        grid,
        queue,
        scale,
        lanes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-11-12T12:00:00Z".parse().unwrap()
    }

    fn mission(id: &str, hours_ago: i64, hours_long: Option<i64>, points: Option<(f64, f64)>, verdict: Option<RatingVerdict>) -> MissionUsageSummary {
        let started = now() - Duration::hours(hours_ago);
        MissionUsageSummary {
            mission_id: id.into(),
            title: format!("mission {id}"),
            status: "stopped".into(),
            started_at: started.to_rfc3339(),
            stopped_at: hours_long.map(|h| (started + Duration::hours(h)).to_rfc3339()),
            last_read_at: None,
            last_call_at: None,
            runtime: Some("muse".into()),
            attributed_low: points.map(|p| p.0),
            attributed_high: points.map(|p| p.1),
            window_points: Some(40.0),
            verdict,
        }
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn lanes_sit_on_the_axis_by_start_and_end_and_are_never_thinner_than_the_minimum() {
        let list = [
            mission("a", 84, Some(12), Some((2.0, 4.0)), None),
            mission("b", 1, Some(0), Some((1.0, 1.0)), None),
        ];
        let t = build(&list, 7, now(), 5.0, Filter::All);
        let a = t.lanes.iter().find(|l| l.mission_id == "a").unwrap();
        assert!(near(a.left, 0.5) && near(a.width, 12.0 / 168.0), "{a:?}");
        let b = t.lanes.iter().find(|l| l.mission_id == "b").unwrap();
        assert!(near(b.width, MIN_BAR), "a mission of no length still shows: {b:?}");
        assert!(b.left + b.width <= 1.0 + 1e-6, "a bar at the right edge is pulled back inside");
    }

    #[test]
    fn an_unstopped_mission_ends_at_its_last_call_not_at_the_time_someone_last_looked() {
        let mut m = mission("a", 48, None, Some((1.0, 1.0)), None);
        m.last_call_at = Some((now() - Duration::hours(24)).to_rfc3339());
        // A reading taken just now, as a backfill takes one, must not stretch the bar to now.
        m.last_read_at = Some(now().to_rfc3339());
        let t = build(&[m.clone()], 7, now(), 5.0, Filter::All);
        assert!(near(t.lanes[0].width, 24.0 / 168.0), "{:?}", t.lanes[0]);
        // With no call recorded, the last reading is the best sign left.
        m.last_call_at = None;
        m.last_read_at = Some((now() - Duration::hours(12)).to_rfc3339());
        assert!(near(build(&[m.clone()], 7, now(), 5.0, Filter::All).lanes[0].width, 36.0 / 168.0));
        // A sign from before it began is ignored.
        m.last_read_at = Some((now() - Duration::hours(60)).to_rfc3339());
        assert!(near(build(&[m], 7, now(), 5.0, Filter::All).lanes[0].width, MIN_BAR));
    }

    #[test]
    fn filters_and_counts_are_taken_over_the_unfiltered_set() {
        let list = [
            mission("dear-bad", 10, Some(1), Some((8.0, 12.0)), Some(RatingVerdict::Down)),
            mission("dear-open", 20, Some(1), Some((6.0, 8.0)), None),
            mission("dear-good", 30, Some(1), Some((6.0, 8.0)), Some(RatingVerdict::Up)),
            mission("cheap", 40, Some(1), Some((0.1, 0.3)), None),
            mission("none", 50, Some(1), None, None),
        ];
        let t = build(&list, 7, now(), 5.0, Filter::Expensive);
        assert_eq!(t.counts, Counts { all: 4, expensive: 3, went_badly: 1, not_rated: 2 });
        assert_eq!(t.lanes.iter().map(|l| l.mission_id.as_str()).collect::<Vec<_>>(), ["dear-bad", "dear-open", "dear-good"]);
        assert_eq!(t.summary.unmeasured, 1);
        let bad = build(&list, 7, now(), 5.0, Filter::WentBadly);
        assert_eq!(bad.lanes.len(), 1);
        assert_eq!(bad.counts.all, 4, "counts do not change with the filter");
        assert_eq!(build(&list, 7, now(), 5.0, Filter::NotRated).lanes.len(), 2);
    }

    #[test]
    fn expensive_missions_that_went_badly_or_were_never_rated_are_flagged_and_queued_dearest_first() {
        let list = [
            mission("bad", 10, Some(1), Some((9.0, 9.0)), Some(RatingVerdict::Down)),
            mission("open", 20, Some(1), Some((12.0, 12.0)), None),
            mission("good", 30, Some(1), Some((20.0, 20.0)), Some(RatingVerdict::Up)),
            mission("cheap-bad", 40, Some(1), Some((0.2, 0.2)), Some(RatingVerdict::Down)),
        ];
        let t = build(&list, 7, now(), 5.0, Filter::All);
        let flag = |id: &str| t.lanes.iter().find(|l| l.mission_id == id).unwrap().flag;
        assert_eq!((flag("bad"), flag("open"), flag("good"), flag("cheap-bad")), (Flag::WentBadly, Flag::NotRated, Flag::None, Flag::None));
        assert_eq!(t.queue.iter().map(|l| l.mission_id.as_str()).collect::<Vec<_>>(), ["open", "bad"]);
        assert_eq!(t.summary.flagged, 2);
        // A lower line catches more.
        assert_eq!(build(&list, 7, now(), 0.1, Filter::All).queue.len(), 3 + 1 - 1 /* good is rated up */ );
    }

    #[test]
    fn the_curve_adds_missions_in_the_order_they_finished_and_ends_at_the_whole_total() {
        let list = [
            mission("a", 30, Some(1), Some((2.0, 2.0)), None),
            mission("b", 10, Some(1), Some((6.0, 6.0)), Some(RatingVerdict::Down)),
            mission("c", 50, Some(1), Some((2.0, 2.0)), None),
        ];
        let t = build(&list, 7, now(), 5.0, Filter::All);
        assert_eq!(t.curve.len(), 3);
        assert!(t.curve.windows(2).all(|pair| pair[0].x <= pair[1].x && pair[0].y <= pair[1].y));
        assert!(near(t.curve[0].y, 0.2) && near(t.curve[1].y, 0.4) && near(t.curve[2].y, 1.0));
        assert_eq!(t.curve[2].flag, Flag::WentBadly, "the dearest one finished last and carries its flag");
        assert_eq!(t.day_labels.len(), 7);
        assert_eq!(t.grid.len(), 8);
        assert!(near(t.grid[0], 0.0) && near(t.grid[7], 1.0));
        assert!((t.summary.points - 10.0).abs() < 1e-9);
    }

    #[test]
    fn points_bars_are_scaled_to_the_largest_high_figure_and_an_empty_period_has_nothing() {
        let list = [mission("big", 5, Some(1), Some((6.0, 8.0)), None), mission("small", 6, Some(1), Some((2.0, 2.0)), None)];
        let t = build(&list, 7, now(), 50.0, Filter::All);
        assert!((t.scale - 8.0).abs() < 1e-9);
        assert!(near(t.lanes[0].solid, 0.75) && near(t.lanes[0].reach, 1.0), "solid to the low end, lighter to the high end");
        assert!(near(t.lanes[1].solid, 0.25) && near(t.lanes[1].reach, 0.25));
        assert!(near(t.lanes[0].width, MIN_BAR) && near(t.lanes[1].width, MIN_BAR), "length of the time bar says nothing about points");
        let empty = build(&[], 30, now(), 5.0, Filter::All);
        assert!(empty.lanes.is_empty() && empty.queue.is_empty() && empty.curve.is_empty());
        assert_eq!(empty.day_labels.len(), 30);
    }
}
