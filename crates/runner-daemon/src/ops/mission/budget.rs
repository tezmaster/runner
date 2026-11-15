// A mission's share of a plan allowance. A runtime reports how much of its
// window the whole account has used, as a whole percent. That total moves for
// every session on the account at once, so the change between two readings is
// an upper bound for one mission, never its share: a mission running beside
// others would be charged for them all.
//
// The share is worked out from tokens instead. At each reading the runtime's
// own logs give what this mission's sessions, and every session on this
// machine, had used since the window began; the mission's points are the
// window's points times its share of those tokens. Parallel missions, chats
// and use outside Runner on this machine are all in the denominator, so they
// cannot be charged to the mission. Whether cached input counts against an
// allowance is not known, so the share is worked out both ways and given as a
// range. What stays invisible is use from another machine on the same account,
// which would push the figure up. The window's total is a whole percent, so
// every figure carries half a point of that total times the share.
//
// Readings are taken by asking each runtime directly, not from the app's usage
// poll, so a daemon with no window open still takes them. A stopped mission is
// never read again: later use of the account is not its. The token logs are
// read where a reading is taken, which can take seconds, so readings are taken
// off the caller's thread.

use std::collections::{BTreeMap, HashSet};

use crate::error::{Error, Result};
use crate::repo;
use crate::repo::mission_review::{BudgetBasisRow, BudgetSampleRow, Tokens};
use crate::runtimes::{TokenScan, TokenSums};
use crate::usage::{UnavailableReason, UsageWindow};
use crate::AppCore;
use chrono::{DateTime, Duration, Utc};
use runner_core::protocol::{BudgetState, CallUsage, MissionBudget, MissionUsageSummary};

/// Why a reading is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    Start,
    Join,
    View,
    Stop,
}

impl SampleKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Join => "join",
            Self::View => "view",
            Self::Stop => "stop",
        }
    }
}

/// A view reading is skipped when the mission was read this recently.
const VIEW_MIN_INTERVAL: Duration = Duration::seconds(60);
/// Two resets times further apart than this are a reset, not rounding.
const RESET_JITTER: Duration = Duration::minutes(5);

/// The allowance window whose total is split by tokens, and its length. Only
/// this one: a runtime that does not say how long its short window is gives no
/// start for counting tokens.
const WEEK_WINDOW: &str = "Week";
const WEEK_LENGTH: Duration = Duration::days(7);

type Reader<'a> = dyn Fn(&str) -> std::result::Result<Vec<UsageWindow>, UnavailableReason> + 'a;
/// What a runtime's logs hold since a time: every session's tokens, and each call of the given sessions.
type History<'a> = dyn Fn(&str, DateTime<Utc>, &HashSet<String>) -> Option<TokenScan> + 'a;

fn parse(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

/// Takes a reading of every runtime the mission uses, through `read`. Returns
/// how many readings were stored. A runtime that cannot be read is skipped:
/// no reading is better than a wrong one.
pub(crate) fn sample_with(
    core: &AppCore,
    mission_id: &str,
    kind: SampleKind,
    now: DateTime<Utc>,
    read: &Reader<'_>,
    history: &History<'_>,
) -> Result<usize> {
    let (runtimes, existing) = {
        let conn = core.db.get()?;
        let mission = super::get(&conn, mission_id)?;
        if mission.archived_at.is_some() && kind != SampleKind::Stop {
            return Ok(0);
        }
        if kind == SampleKind::View && !repo::mission_review::has_live_session(&conn, mission_id)? {
            // A mission that is not running is finished: its share stops growing.
            return Ok(0);
        }
        (
            repo::mission_review::mission_runtimes(&conn, mission_id)?,
            repo::mission_review::list_budget_samples(&conn, mission_id)?,
        )
    };
    if kind == SampleKind::View {
        let latest = existing.iter().filter_map(|row| parse(&row.read_at)).max();
        if latest.is_some_and(|latest| now - latest < VIEW_MIN_INTERVAL) {
            return Ok(0);
        }
    }
    let mut stored = 0;
    for runtime in runtimes {
        let Ok(windows) = read(&runtime) else {
            continue;
        };
        let has_baseline = existing.iter().any(|row| row.runtime == runtime);
        let kind = match (has_baseline, kind) {
            (false, SampleKind::Start) => SampleKind::Start,
            (false, _) => SampleKind::Join,
            (true, SampleKind::Start | SampleKind::Join) => SampleKind::View,
            (true, other) => other,
        };
        let conn = core.db.get()?;
        let week_resets = windows
            .iter()
            .find(|window| window.name == WEEK_WINDOW)
            .and_then(|window| window.resets_at);
        for window in windows {
            repo::mission_review::insert_budget_sample(
                &conn,
                mission_id,
                &BudgetSampleRow {
                    runtime: runtime.clone(),
                    window_name: window.name,
                    used_percent: window.used_percent,
                    resets_at: window.resets_at.map(|time| time.to_rfc3339()),
                    read_at: now.to_rfc3339(),
                    kind: kind.as_str().to_owned(),
                },
            )?;
            stored += 1;
        }
        if let Some(resets_at) = week_resets {
            let week_start = resets_at - WEEK_LENGTH;
            let own: HashSet<String> =
                repo::mission_review::mission_session_keys(&conn, mission_id, &runtime)?
                    .into_iter()
                    .collect();
            if let Some(scan) = history(&runtime, week_start, &own) {
                let (mut mission, mut machine) = (TokenSums::default(), TokenSums::default());
                for (key, sums) in &scan.by_session {
                    machine.add(sums);
                    if own.contains(key) {
                        mission.add(sums);
                    }
                }
                let tokens = |sums: TokenSums| Tokens {
                    input: sums.input,
                    output: sums.output,
                    cached: sums.cached,
                    reasoning: sums.reasoning,
                };
                let calls: Vec<repo::mission_review::CallRow> = scan
                    .calls
                    .iter()
                    .filter_map(|call| {
                        Some(repo::mission_review::CallRow {
                            usage_id: call.usage_id.clone(),
                            runtime: runtime.clone(),
                            session_key: call.session_key.clone(),
                            at: DateTime::from_timestamp_micros(call.at_us)?.to_rfc3339(),
                            tokens: tokens(call.tokens),
                        })
                    })
                    .collect();
                repo::mission_review::insert_calls(&conn, mission_id, &calls)?;
                repo::mission_review::insert_budget_basis(
                    &conn,
                    mission_id,
                    &BudgetBasisRow {
                        runtime: runtime.clone(),
                        read_at: now.to_rfc3339(),
                        week_start: week_start.to_rfc3339(),
                        mission: tokens(mission),
                        machine: tokens(machine),
                    },
                )?;
            }
        }
    }
    Ok(stored)
}

/// The runtime's own token history, if it has one. A failure is no history.
fn read_history(runtime: &str, since: DateTime<Utc>, keys: &HashSet<String>) -> Option<TokenScan> {
    let runtime = crate::model::Runtime::parse(runtime)?;
    let source = crate::runtimes::adapter(runtime).token_history()?;
    match (source.read)(since, keys) {
        Ok(sums) => Some(sums),
        Err(error) => {
            log::warn!("token history for {runtime:?} unavailable: {error}");
            None
        }
    }
}

/// Asks the runtime's own reader, as the usage poll does, but for one runtime
/// and without depending on the app having told the daemon to watch it.
fn read_runtime(
    core: &AppCore,
    runtime: &str,
) -> std::result::Result<Vec<UsageWindow>, UnavailableReason> {
    let runtime = crate::model::Runtime::parse(runtime).ok_or(UnavailableReason::InvalidResponse)?;
    let source = crate::runtimes::adapter(runtime)
        .usage()
        .ok_or(UnavailableReason::InvalidResponse)?;
    let statuses = crate::runtime_status::status_list(
        &core.db,
        &core.runtime_shell_env,
        &core.runtime_discovery,
    )
    .map_err(|_| UnavailableReason::InvalidResponse)?;
    let status = statuses
        .runtimes
        .into_iter()
        .find(|status| status.name == runtime)
        .ok_or(UnavailableReason::InvalidResponse)?;
    let command = status
        .effective_command
        .ok_or(UnavailableReason::InvalidResponse)?;
    let env = core.runtime_shell_env.read().unwrap().clone();
    (source.fetch)(&command, &env, false).map(|reading| reading.windows)
}

pub fn mission_budget_sample(core: &AppCore, mission_id: &str, kind: SampleKind) -> Result<usize> {
    if !core.usage.fetches() {
        // A core that does not run agent CLIs (every test core) takes no readings.
        return Ok(0);
    }
    sample_with(
        core,
        mission_id,
        kind,
        Utc::now(),
        &|runtime| read_runtime(core, runtime),
        &read_history,
    )
}

/// Takes a reading without holding the caller up: starting, stopping and
/// adding a role must not wait on a runtime's CLI.
pub fn sample_in_background(core: AppCore, mission_id: String, kind: SampleKind) {
    if !core.usage.fetches() {
        return;
    }
    std::thread::spawn(move || {
        if let Err(error) = mission_budget_sample(&core, &mission_id, kind) {
            log::warn!("budget reading for mission {mission_id} failed: {error}");
        }
    });
}

/// A mission's points of one allowance window, split by tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Attribution {
    pub low: f64,
    pub high: f64,
    pub margin: f64,
    pub window_points: f64,
    pub share_low: f64,
    pub share_high: f64,
}

/// `points` is the window's whole-percent total at a reading; `mission` and
/// `machine` are token totals since the window began. The share is taken with
/// cached input counted in full and again with it free: a cache hit may or may
/// not count against an allowance. `None` when the machine used nothing under
/// both readings of the share.
pub(crate) fn split_by_tokens(points: f64, mission: Tokens, machine: Tokens) -> Option<Attribution> {
    let sums = |tokens: Tokens| TokenSums {
        input: tokens.input,
        output: tokens.output,
        cached: tokens.cached,
        reasoning: tokens.reasoning,
    };
    let (m, all) = (sums(mission), sums(machine));
    let shares: Vec<f64> = [
        (m.total_basis(), all.total_basis()),
        (m.uncached_basis(), all.uncached_basis()),
    ]
    .into_iter()
    .filter(|(_, all)| *all > 0)
    .map(|(m, all)| (m as f64 / all as f64).clamp(0.0, 1.0))
    .collect();
    let share_low = shares.iter().copied().reduce(f64::min)?;
    let share_high = shares.iter().copied().reduce(f64::max)?;
    Some(Attribution {
        low: points * share_low,
        high: points * share_high,
        // The window's total is a whole percent: half a point either way, times the share.
        margin: 0.5 * share_high,
        window_points: points,
        share_low,
        share_high,
    })
}

/// A call's points under each reading of the allowance: `(cached counted in
/// full, cached counted as free)`. A reading with nothing on the machine to
/// divide by is `None`.
pub(crate) fn basis_points(points: f64, tokens: Tokens, machine: Tokens) -> (Option<f64>, Option<f64>) {
    let sums = |tokens: Tokens| TokenSums {
        input: tokens.input,
        output: tokens.output,
        cached: tokens.cached,
        reasoning: tokens.reasoning,
    };
    let (m, all) = (sums(tokens), sums(machine));
    let part = |mine: i64, whole: i64| (whole > 0).then(|| points * (mine as f64 / whole as f64).clamp(0.0, 1.0));
    (
        part(m.total_basis(), all.total_basis()),
        part(m.uncached_basis(), all.uncached_basis()),
    )
}

/// Adds the attributions of a mission that ran across several windows. The
/// share and total shown are the latest window's.
pub(crate) fn sum_attributions(parts: &[Attribution]) -> Option<Attribution> {
    let latest = *parts.last()?;
    Some(Attribution {
        low: parts.iter().map(|part| part.low).sum(),
        high: parts.iter().map(|part| part.high).sum(),
        margin: parts.iter().map(|part| part.margin).sum(),
        ..latest
    })
}

/// Each call of the mission with the points it used. A call's points are the
/// window's points at the latest reading of its week times the call's share of
/// that week's machine tokens, worked out both ways like the mission's total.
pub fn mission_calls(conn: &rusqlite::Connection, mission_id: &str) -> Result<Vec<CallUsage>> {
    let calls = repo::mission_review::list_calls(conn, mission_id)?;
    if calls.is_empty() {
        return Ok(Vec::new());
    }
    let basis = repo::mission_review::list_budget_basis(conn, mission_id)?;
    let samples = repo::mission_review::list_budget_samples(conn, mission_id)?;
    // The latest basis row per runtime and week, with the window's points at it.
    let mut weeks: Vec<(String, DateTime<Utc>, &BudgetBasisRow, f64)> = Vec::new();
    for row in &basis {
        let Some(start) = parse(&row.week_start) else {
            continue;
        };
        let Some(points) = samples
            .iter()
            .find(|s| s.runtime == row.runtime && s.window_name == WEEK_WINDOW && s.read_at == row.read_at)
            .map(|s| s.used_percent)
        else {
            continue;
        };
        match weeks
            .iter_mut()
            .find(|(runtime, week, _, _)| *runtime == row.runtime && *week == start)
        {
            Some(kept) if kept.2.read_at < row.read_at => *kept = (row.runtime.clone(), start, row, points),
            Some(_) => {}
            None => weeks.push((row.runtime.clone(), start, row, points)),
        }
    }
    Ok(calls
        .into_iter()
        .map(|call| {
            let at = parse(&call.at);
            let week = at.and_then(|at| {
                weeks
                    .iter()
                    .find(|(runtime, start, _, _)| {
                        *runtime == call.runtime && *start <= at && at < *start + WEEK_LENGTH
                    })
            });
            let (in_full, free) = week
                .map(|(_, _, row, points)| basis_points(*points, call.tokens, row.machine))
                .unwrap_or((None, None));
            CallUsage {
                usage_id: call.usage_id,
                runtime: call.runtime,
                session_key: call.session_key,
                at: call.at,
                input_tokens: call.tokens.input,
                output_tokens: call.tokens.output,
                cached_tokens: call.tokens.cached,
                reasoning_tokens: call.tokens.reasoning,
                points_cached_in_full: in_full,
                points_cached_free: free,
            }
        })
        .collect())
}

/// What a backfill did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackfillOutcome {
    /// Missions now measured against the current weekly window.
    pub measured: usize,
    /// Missions left alone: nothing of theirs in the window, or already measured.
    pub skipped: usize,
}

/// Measures missions that already ran, from one reading taken now. A runtime's
/// allowance is only known as it stands now, so a mission can only be split
/// against the week that is still open: its points are the window's total
/// times its share of the week's tokens, which is the same figure a reading
/// taken as it finished would have given. Missions with nothing in the open
/// week are skipped, as are those that already have a measure for it: a real
/// reading is never replaced by an estimate.
pub(crate) fn backfill_with(
    core: &AppCore,
    days: u32,
    now: DateTime<Utc>,
    read: &Reader<'_>,
    history: &History<'_>,
) -> Result<BackfillOutcome> {
    let since = (now - Duration::days(i64::from(days.clamp(1, 90)))).to_rfc3339();
    let conn = core.db.get()?;
    let ids: Vec<String> = {
        let mut stmt = conn.prepare("SELECT id FROM missions WHERE started_at >= ?1 ORDER BY started_at")?;
        let rows = stmt.query_map(rusqlite::params![since], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut by_runtime: BTreeMap<String, Vec<(String, HashSet<String>)>> = BTreeMap::new();
    for id in &ids {
        for runtime in repo::mission_review::mission_runtimes(&conn, id)? {
            let keys: HashSet<String> = repo::mission_review::mission_session_keys(&conn, id, &runtime)?
                .into_iter()
                .collect();
            by_runtime.entry(runtime).or_default().push((id.clone(), keys));
        }
    }
    let mut outcome = BackfillOutcome::default();
    let mut measured: HashSet<String> = HashSet::new();
    for (runtime, missions) in by_runtime {
        let Ok(windows) = read(&runtime) else {
            continue;
        };
        let Some(week) = windows.iter().find(|window| window.name == WEEK_WINDOW) else {
            continue;
        };
        let Some(resets_at) = week.resets_at else {
            continue;
        };
        let week_start = resets_at - WEEK_LENGTH;
        let all_keys: HashSet<String> = missions.iter().flat_map(|(_, keys)| keys.iter().cloned()).collect();
        let Some(scan) = history(&runtime, week_start, &all_keys) else {
            continue;
        };
        let mut machine = TokenSums::default();
        for sums in scan.by_session.values() {
            machine.add(sums);
        }
        let tokens = |sums: TokenSums| Tokens {
            input: sums.input,
            output: sums.output,
            cached: sums.cached,
            reasoning: sums.reasoning,
        };
        for (mission_id, keys) in missions {
            let already = repo::mission_review::list_budget_basis(&conn, &mission_id)?
                .iter()
                .any(|row| row.runtime == runtime && row.week_start == week_start.to_rfc3339());
            let mut own = TokenSums::default();
            for (key, sums) in &scan.by_session {
                if keys.contains(key) {
                    own.add(sums);
                }
            }
            if already || own.total_basis() == 0 {
                outcome.skipped += 1;
                continue;
            }
            for window in &windows {
                repo::mission_review::insert_budget_sample(
                    &conn,
                    &mission_id,
                    &BudgetSampleRow {
                        runtime: runtime.clone(),
                        window_name: window.name.clone(),
                        used_percent: window.used_percent,
                        resets_at: window.resets_at.map(|time| time.to_rfc3339()),
                        read_at: now.to_rfc3339(),
                        kind: SampleKind::Stop.as_str().to_owned(),
                    },
                )?;
            }
            let calls: Vec<repo::mission_review::CallRow> = scan
                .calls
                .iter()
                .filter(|call| keys.contains(&call.session_key))
                .filter_map(|call| {
                    Some(repo::mission_review::CallRow {
                        usage_id: call.usage_id.clone(),
                        runtime: runtime.clone(),
                        session_key: call.session_key.clone(),
                        at: DateTime::from_timestamp_micros(call.at_us)?.to_rfc3339(),
                        tokens: tokens(call.tokens),
                    })
                })
                .collect();
            repo::mission_review::insert_calls(&conn, &mission_id, &calls)?;
            repo::mission_review::insert_budget_basis(
                &conn,
                &mission_id,
                &BudgetBasisRow {
                    runtime: runtime.clone(),
                    read_at: now.to_rfc3339(),
                    week_start: week_start.to_rfc3339(),
                    mission: tokens(own),
                    machine: tokens(machine),
                },
            )?;
            if measured.insert(mission_id) {
                outcome.measured += 1;
            }
        }
    }
    Ok(outcome)
}

pub fn mission_usage_backfill(core: &AppCore, days: u32) -> Result<BackfillOutcome> {
    if !core.usage.fetches() {
        return Ok(BackfillOutcome::default());
    }
    backfill_with(core, days, Utc::now(), &|runtime| read_runtime(core, runtime), &read_history)
}

/// Missions started in the last `days` days, newest first, each with its share
/// of the weekly allowance (when it has one) and its rating. At most 300.
pub fn mission_usage_list(core: &AppCore, days: u32) -> Result<Vec<MissionUsageSummary>> {
    mission_usage_list_at(core, days, Utc::now())
}

pub(crate) fn mission_usage_list_at(core: &AppCore, days: u32, now: DateTime<Utc>) -> Result<Vec<MissionUsageSummary>> {
    let days = days.clamp(1, 90);
    let since = (now - Duration::days(i64::from(days))).to_rfc3339();
    let conn = core.db.get()?;
    type Row = (String, String, String, String, Option<String>);
    let rows: Vec<Row> = {
        let mut stmt = conn.prepare(
            "SELECT id, title, status, started_at, stopped_at FROM missions
             WHERE started_at >= ?1 ORDER BY started_at DESC LIMIT 300",
        )?;
        let rows = stmt.query_map(rusqlite::params![since], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut out = Vec::with_capacity(rows.len());
    for (mission_id, title, status, started_at, stopped_at) in rows {
        let budgets = mission_budgets(&conn, &mission_id)?;
        let week = budgets.iter().find(|budget| {
            budget.window == WEEK_WINDOW && budget.attributed_low.is_some() && budget.attributed_high.is_some()
        });
        out.push(MissionUsageSummary {
            verdict: repo::mission_review::get_rating(&conn, &mission_id)?.map(|rating| rating.verdict),
            runtime: week.map(|budget| budget.runtime.clone()),
            attributed_low: week.and_then(|budget| budget.attributed_low),
            attributed_high: week.and_then(|budget| budget.attributed_high),
            window_points: week.and_then(|budget| budget.window_points),
            last_read_at: week.map(|budget| budget.latest_read_at.clone()),
            last_call_at: conn
                .query_row(
                    "SELECT MAX(at) FROM mission_usage_call WHERE mission_id = ?1",
                    rusqlite::params![&mission_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .map_err(|error| Error::msg(error.to_string()))?,
            mission_id,
            title,
            status,
            started_at,
            stopped_at,
        });
    }
    Ok(out)
}

/// The mission's share of each allowance window it has readings for.
pub fn mission_budgets(conn: &rusqlite::Connection, mission_id: &str) -> Result<Vec<MissionBudget>> {
    let samples = repo::mission_review::list_budget_samples(conn, mission_id)?;
    let basis = repo::mission_review::list_budget_basis(conn, mission_id)?;
    let mut out = Vec::new();
    let mut keys: Vec<(String, String)> = samples
        .iter()
        .map(|row| (row.runtime.clone(), row.window_name.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    for (runtime, window) in keys {
        let rows: Vec<&BudgetSampleRow> = samples
            .iter()
            .filter(|row| row.runtime == runtime && row.window_name == window)
            .collect();
        let (Some(first), Some(latest)) = (rows.first(), rows.last()) else {
            continue;
        };
        let first_at = parse(&first.read_at);
        let latest_at = parse(&latest.read_at);
        let reset_moved = match (
            first.resets_at.as_deref().and_then(parse),
            latest.resets_at.as_deref().and_then(parse),
        ) {
            (Some(a), Some(b)) => (b - a).abs() > RESET_JITTER,
            _ => false,
        };
        let spanned_reset = reset_moved || latest.used_percent < first.used_percent;
        let state = if rows.len() < 2 {
            BudgetState::NotYetMeasured
        } else if spanned_reset {
            BudgetState::SpannedReset
        } else {
            BudgetState::Measured
        };
        let other_sessions = match (first_at, latest_at) {
            (Some(from), Some(to)) => {
                repo::mission_review::other_sessions_on_runtime(conn, mission_id, &runtime)?
                    .into_iter()
                    .filter(|(started, stopped, live)| {
                        let started = started.as_deref().and_then(parse);
                        let stopped = stopped.as_deref().and_then(parse);
                        let began_before_end = started.is_none_or(|started| started <= to);
                        let ended_after_start = match stopped {
                            Some(stopped) => stopped >= from,
                            None => *live,
                        };
                        began_before_end && ended_after_start
                    })
                    .count() as u32
            }
            _ => 0,
        };
        // Split the window's total by tokens, window by window: the latest
        // reading in each weekly window has the most tokens counted.
        let attribution = if window == WEEK_WINDOW {
            let mut by_week: BTreeMap<&str, &BudgetBasisRow> = BTreeMap::new();
            for row in basis.iter().filter(|row| row.runtime == runtime) {
                let newer = by_week
                    .get(row.week_start.as_str())
                    .is_none_or(|kept| kept.read_at < row.read_at);
                if newer {
                    by_week.insert(row.week_start.as_str(), row);
                }
            }
            let mut parts = Vec::new();
            for row in by_week.values() {
                let points = samples
                    .iter()
                    .find(|s| {
                        s.runtime == runtime && s.window_name == WEEK_WINDOW && s.read_at == row.read_at
                    })
                    .map(|s| s.used_percent);
                if let Some(part) = points.and_then(|p| split_by_tokens(p, row.mission, row.machine)) {
                    parts.push((row.week_start.as_str(), part));
                }
            }
            parts.sort_by_key(|(week, _)| *week);
            sum_attributions(&parts.into_iter().map(|(_, part)| part).collect::<Vec<_>>())
        } else {
            None
        };
        out.push(MissionBudget {
            runtime,
            window,
            attributed_low: attribution.map(|a| a.low),
            attributed_high: attribution.map(|a| a.high),
            rounding_margin: attribution.map(|a| a.margin),
            window_points: attribution.map(|a| a.window_points),
            share_low: attribution.map(|a| a.share_low),
            share_high: attribution.map(|a| a.share_high),
            state,
            first_percent: first.used_percent,
            latest_percent: latest.used_percent,
            used_points: (state == BudgetState::Measured)
                .then(|| latest.used_percent - first.used_percent),
            first_read_at: first.read_at.clone(),
            latest_read_at: latest.read_at.clone(),
            readings: rows.len() as u32,
            other_sessions,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::mission::{start, MissionStart};
    use crate::ops::project::ProjectScope;
    use crate::test_support::{insert_session_row, test_core_in, test_session_row};
    use crate::model::SessionStatus;
    use std::cell::RefCell;

    struct Fixture {
        _dir: tempfile::TempDir,
        core: AppCore,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core_in(dir.path().to_path_buf());
        Fixture { _dir: dir, core }
    }

    fn at(minute: i64) -> DateTime<Utc> {
        "2026-11-12T10:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::minutes(minute)
    }

    impl Fixture {
        fn mission(&self) -> String {
            start(
                &mut self.core.db.get().unwrap(),
                &self.core.app_data_dir,
                MissionStart {
                    crew_id: None,
                    scope: ProjectScope::Root,
                    title: "Budget".into(),
                    goal_override: None,
                    cwd: Some("/tmp".into()),
                    started_by_session_id: None,
                    no_report: false,
                },
            )
            .unwrap()
            .mission
            .id
        }

        fn session(&self, id: &str, mission: Option<&str>, runtime: &str, status: SessionStatus) {
            let mut row = test_session_row(id, status);
            row.agent_runtime = Some(runtime.into());
            row.mission_id = mission.map(str::to_owned);
            if mission.is_some() {
                row.slot_id = Some("slot".into());
            }
            insert_session_row(&self.core.db.get().unwrap(), &row).unwrap();
        }

        fn sample(&self, mission: &str, kind: SampleKind, minute: i64, percent: f64, resets: &str) -> usize {
            let windows = vec![UsageWindow {
                name: "Week".into(),
                used_percent: percent,
                resets_at: Some(resets.parse().unwrap()),
            }];
            sample_with(&self.core, mission, kind, at(minute), &|_| Ok(windows.clone()), &|_, _, _| None).unwrap()
        }

        fn budgets(&self, mission: &str) -> Vec<MissionBudget> {
            mission_budgets(&self.core.db.get().unwrap(), mission).unwrap()
        }
    }

    const RESETS: &str = "2026-11-17T00:00:00Z";

    #[test]
    fn one_reading_is_not_a_figure_two_are() {
        let f = fixture();
        let m = f.mission();
        f.session("s1", Some(&m), "muse", SessionStatus::Running);
        assert_eq!(f.sample(&m, SampleKind::Start, 0, 40.0, RESETS), 1);
        let one = &f.budgets(&m)[0];
        assert_eq!((one.state, one.used_points, one.readings), (BudgetState::NotYetMeasured, None, 1));
        f.sample(&m, SampleKind::View, 5, 49.0, RESETS);
        let two = &f.budgets(&m)[0];
        assert_eq!(two.state, BudgetState::Measured);
        assert_eq!(two.used_points, Some(9.0));
        assert_eq!((two.first_percent, two.latest_percent), (40.0, 49.0));
    }

    #[test]
    fn a_reset_in_between_gives_no_figure() {
        let f = fixture();
        let m = f.mission();
        f.session("s1", Some(&m), "muse", SessionStatus::Running);
        f.sample(&m, SampleKind::Start, 0, 90.0, RESETS);
        f.sample(&m, SampleKind::View, 10, 3.0, "2026-11-24T00:00:00Z");
        let b = &f.budgets(&m)[0];
        assert_eq!((b.state, b.used_points), (BudgetState::SpannedReset, None));
        // A lower percent alone, with the reset time unchanged, is also a reset.
        let g = fixture();
        let n = g.mission();
        g.session("s2", Some(&n), "muse", SessionStatus::Running);
        g.sample(&n, SampleKind::Start, 0, 50.0, RESETS);
        g.sample(&n, SampleKind::View, 10, 2.0, RESETS);
        assert_eq!(g.budgets(&n)[0].state, BudgetState::SpannedReset);
    }

    #[test]
    fn view_readings_are_rate_limited_and_need_a_live_session() {
        let f = fixture();
        let m = f.mission();
        f.session("s1", Some(&m), "muse", SessionStatus::Running);
        f.sample(&m, SampleKind::Start, 0, 40.0, RESETS);
        assert_eq!(f.sample(&m, SampleKind::View, 0, 41.0, RESETS), 0, "within a minute of the last reading");
        assert_eq!(f.sample(&m, SampleKind::View, 2, 41.0, RESETS), 1);
        // The same mission with its only session stopped is finished: never read again.
        let g = fixture();
        let n = g.mission();
        g.session("s2", Some(&n), "muse", SessionStatus::Stopped);
        assert_eq!(g.sample(&n, SampleKind::View, 0, 40.0, RESETS), 0);
        assert!(g.budgets(&n).is_empty());
    }

    #[test]
    fn a_stop_reading_is_taken_after_the_sessions_end_and_then_nothing_more() {
        let f = fixture();
        let m = f.mission();
        f.session("s1", Some(&m), "muse", SessionStatus::Running);
        f.sample(&m, SampleKind::Start, 0, 40.0, RESETS);
        f.core.db.get().unwrap().execute("UPDATE sessions SET status = 'stopped' WHERE id = 's1'", []).unwrap();
        assert_eq!(f.sample(&m, SampleKind::Stop, 20, 55.0, RESETS), 1);
        assert_eq!(f.budgets(&m)[0].used_points, Some(15.0));
        assert_eq!(f.sample(&m, SampleKind::View, 40, 70.0, RESETS), 0, "no reading after the stop");
    }

    #[test]
    fn a_runtime_that_joins_late_is_measured_from_when_it_joined() {
        let f = fixture();
        let m = f.mission();
        f.session("s1", Some(&m), "muse", SessionStatus::Running);
        f.sample(&m, SampleKind::Start, 0, 40.0, RESETS);
        f.session("s2", Some(&m), "codex", SessionStatus::Running);
        let seen = RefCell::new(Vec::new());
        let windows = vec![UsageWindow { name: "Week".into(), used_percent: 10.0, resets_at: None }];
        sample_with(&f.core, &m, SampleKind::Join, at(30), &|runtime| {
            seen.borrow_mut().push(runtime.to_owned());
            Ok(windows.clone())
        }, &|_, _, _| None)
        .unwrap();
        assert_eq!(*seen.borrow(), ["codex", "muse"]);
        let by_runtime = |name: &str| f.budgets(&m).into_iter().find(|b| b.runtime == name).unwrap();
        assert_eq!(by_runtime("codex").readings, 1, "the first reading of a runtime is its baseline");
        assert_eq!(by_runtime("muse").readings, 2);
    }

    #[test]
    fn other_runner_sessions_that_overlapped_are_counted() {
        let f = fixture();
        let m = f.mission();
        f.session("s1", Some(&m), "muse", SessionStatus::Running);
        f.session("chat", None, "muse", SessionStatus::Running);
        f.session("other-runtime", None, "codex", SessionStatus::Running);
        f.session("ended-before", None, "muse", SessionStatus::Stopped);
        f.session("starts-after", None, "muse", SessionStatus::Running);
        {
            let conn = f.core.db.get().unwrap();
            let set = |id: &str, started: &str, stopped: Option<&str>| {
                conn.execute(
                    "UPDATE sessions SET started_at = ?2, stopped_at = ?3 WHERE id = ?1",
                    rusqlite::params![id, started, stopped],
                )
                .unwrap();
            };
            set("s1", "2026-11-12T09:59:00+00:00", None);
            set("chat", "2026-11-12T09:00:00+00:00", None);
            set("other-runtime", "2026-11-12T09:00:00+00:00", None);
            set("ended-before", "2026-11-12T08:00:00+00:00", Some("2026-11-12T09:30:00+00:00"));
            set("starts-after", "2026-11-12T12:00:00+00:00", None);
        }
        f.sample(&m, SampleKind::Start, 0, 40.0, RESETS);
        f.sample(&m, SampleKind::View, 5, 44.0, RESETS);
        assert_eq!(
            f.budgets(&m)[0].other_sessions,
            1,
            "only the chat overlapped: the codex one is another allowance, one ended before and one began after"
        );
    }

    #[test]
    fn an_unreadable_runtime_stores_nothing() {
        let f = fixture();
        let m = f.mission();
        f.session("s1", Some(&m), "muse", SessionStatus::Running);
        let stored = sample_with(&f.core, &m, SampleKind::Start, at(0), &|_| Err(UnavailableReason::SignIn), &|_, _, _| None).unwrap();
        assert_eq!(stored, 0);
        assert!(f.budgets(&m).is_empty());
    }

    fn tokens(input: i64, output: i64, cached: i64) -> Tokens {
        Tokens { input, output, cached, reasoning: 0 }
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn a_mission_gets_its_share_of_the_windows_points() {
        let a = split_by_tokens(40.0, tokens(100, 0, 0), tokens(1000, 0, 0)).unwrap();
        assert!(near(a.low, 4.0) && near(a.high, 4.0));
        assert!(near(a.margin, 0.05), "half a point of the total, times a 10% share");
        assert!(near(a.share_low, 0.1) && near(a.window_points, 40.0));
    }

    #[test]
    fn the_range_covers_cached_input_counting_or_not() {
        // Mission: 100 input (90 cached) + 10 output. Machine: 1000 input (500 cached) + 100 output.
        let a = split_by_tokens(30.0, tokens(100, 10, 90), tokens(1000, 100, 500)).unwrap();
        assert!(near(a.share_high, 110.0 / 1100.0), "cached counted in full");
        assert!(near(a.share_low, 20.0 / 600.0), "cached counted as free");
        assert!(near(a.low, 30.0 * 20.0 / 600.0) && near(a.high, 3.0));
    }

    #[test]
    fn a_window_with_no_machine_tokens_gives_no_split() {
        assert_eq!(split_by_tokens(10.0, tokens(0, 0, 0), tokens(0, 0, 0)), None);
        // Output only on the machine: the cached-free reading still has a denominator.
        assert!(split_by_tokens(10.0, tokens(0, 5, 0), tokens(0, 50, 0)).is_some());
    }

    fn with_history(f: &Fixture, m: &str, minute: i64, percent: f64, resets: &str, history: &[(&str, TokenSums)]) {
        let windows = vec![UsageWindow {
            name: "Week".into(),
            used_percent: percent,
            resets_at: Some(resets.parse().unwrap()),
        }];
        let by_session: BTreeMap<String, TokenSums> =
            history.iter().map(|(key, sums)| ((*key).to_owned(), *sums)).collect();
        let scan = TokenScan { by_session, calls: Vec::new() };
        sample_with(&f.core, m, SampleKind::View, at(minute), &|_| Ok(windows.clone()), &|_, _, _| Some(scan.clone())).unwrap();
    }

    fn sums(input: i64, output: i64, cached: i64) -> TokenSums {
        TokenSums { input, output, cached, reasoning: 0 }
    }

    fn keyed_session(f: &Fixture, id: &str, mission: Option<&str>, key: &str) {
        f.session(id, mission, "muse", SessionStatus::Running);
        f.core
            .db
            .get()
            .unwrap()
            .execute("UPDATE sessions SET agent_session_key = ?2 WHERE id = ?1", rusqlite::params![id, key])
            .unwrap();
    }

    #[test]
    fn parallel_sessions_are_in_the_denominator_not_charged_to_the_mission() {
        let f = fixture();
        let m = f.mission();
        keyed_session(&f, "s1", Some(&m), "key-mine");
        keyed_session(&f, "chat", None, "key-chat");
        // Another mission's sessions and a terminal session Runner never started.
        let other = f.mission();
        keyed_session(&f, "s-other", Some(&other), "key-other-mission");
        with_history(
            &f,
            &m,
            0,
            40.0,
            RESETS,
            &[
                ("key-mine", sums(100, 0, 0)),
                ("key-chat", sums(300, 0, 0)),
                ("key-other-mission", sums(500, 0, 0)),
                ("key-from-a-terminal", sums(100, 0, 0)),
            ],
        );
        let b = &f.budgets(&m)[0];
        assert_eq!(b.state, BudgetState::NotYetMeasured, "one reading is no before-and-after");
        assert!(near(b.attributed_low.unwrap(), 4.0), "100 of 1000 machine tokens, of 40 points: {b:?}");
        assert!(near(b.window_points.unwrap(), 40.0));
    }

    #[test]
    fn the_latest_reading_in_a_week_is_the_one_used() {
        let f = fixture();
        let m = f.mission();
        keyed_session(&f, "s1", Some(&m), "key-mine");
        with_history(&f, &m, 0, 10.0, RESETS, &[("key-mine", sums(50, 0, 0)), ("key-x", sums(450, 0, 0))]);
        with_history(&f, &m, 30, 20.0, RESETS, &[("key-mine", sums(400, 0, 0)), ("key-x", sums(600, 0, 0))]);
        let b = &f.budgets(&m)[0];
        assert!(near(b.attributed_low.unwrap(), 20.0 * 0.4), "the later reading counts, not both: {b:?}");
    }

    #[test]
    fn a_mission_across_two_weeks_adds_each_weeks_share() {
        let f = fixture();
        let m = f.mission();
        keyed_session(&f, "s1", Some(&m), "key-mine");
        with_history(&f, &m, 0, 50.0, "2026-11-13T00:00:00Z", &[("key-mine", sums(100, 0, 0)), ("key-x", sums(100, 0, 0))]);
        with_history(&f, &m, 60 * 24 * 3, 5.0, "2026-11-20T00:00:00Z", &[("key-mine", sums(50, 0, 0)), ("key-x", sums(150, 0, 0))]);
        let b = &f.budgets(&m)[0];
        assert!(near(b.attributed_low.unwrap(), 50.0 * 0.5 + 5.0 * 0.25), "{b:?}");
        assert!(near(b.window_points.unwrap(), 5.0), "the latest window's total is shown");
    }

    #[test]
    fn a_runtime_without_token_history_still_shows_the_before_and_after() {
        let f = fixture();
        let m = f.mission();
        f.session("s1", Some(&m), "muse", SessionStatus::Running);
        f.sample(&m, SampleKind::Start, 0, 40.0, RESETS);
        f.sample(&m, SampleKind::View, 5, 43.0, RESETS);
        let b = &f.budgets(&m)[0];
        assert_eq!((b.state, b.used_points), (BudgetState::Measured, Some(3.0)));
        assert_eq!((b.attributed_low, b.attributed_high), (None, None));
    }

    fn call(id: &str, key: &str, at_us: i64, input: i64, output: i64, cached: i64) -> crate::runtimes::CallRecord {
        crate::runtimes::CallRecord {
            usage_id: id.to_owned(),
            session_key: key.to_owned(),
            at_us,
            tokens: sums(input, output, cached),
        }
    }

    fn with_calls(f: &Fixture, m: &str, minute: i64, percent: f64, machine: &[(&str, TokenSums)], calls: Vec<crate::runtimes::CallRecord>) {
        let windows = vec![UsageWindow { name: "Week".into(), used_percent: percent, resets_at: Some(RESETS.parse().unwrap()) }];
        let by_session: BTreeMap<String, TokenSums> = machine.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect();
        let scan = TokenScan { by_session, calls };
        sample_with(&f.core, m, SampleKind::View, at(minute), &|_| Ok(windows.clone()), &|_, _, _| Some(scan.clone())).unwrap();
    }

    #[test]
    fn each_call_gets_its_share_of_the_points_and_they_add_up_to_the_mission() {
        let f = fixture();
        let m = f.mission();
        keyed_session(&f, "s1", Some(&m), "key-mine");
        let t = at(1).timestamp_micros();
        with_calls(
            &f,
            &m,
            2,
            40.0,
            &[("key-mine", sums(300, 0, 0)), ("key-x", sums(700, 0, 0))],
            vec![call("u1", "key-mine", t, 100, 0, 0), call("u2", "key-mine", t + 1, 200, 0, 0)],
        );
        let review = f.budgets(&m);
        let calls = mission_calls(&f.core.db.get().unwrap(), &m).unwrap();
        assert_eq!(calls.len(), 2);
        assert!(near(calls[0].points_cached_in_full.unwrap(), 40.0 * 100.0 / 1000.0), "{calls:?}");
        assert!(near(calls[1].points_cached_free.unwrap(), 40.0 * 200.0 / 1000.0));
        let sum: f64 = calls.iter().map(|c| c.points_cached_in_full.unwrap()).sum();
        assert!(near(sum, review[0].attributed_low.unwrap()), "calls add up to the mission's figure");
    }

    #[test]
    fn with_cached_input_the_calls_added_under_each_reading_equal_the_missions_range() {
        let f = fixture();
        let m = f.mission();
        keyed_session(&f, "s1", Some(&m), "key-mine");
        let t = at(1).timestamp_micros();
        // Heavily cached calls: the two readings of the allowance differ a lot.
        with_calls(
            &f,
            &m,
            2,
            30.0,
            &[("key-mine", sums(300, 20, 270)), ("key-x", sums(700, 80, 350))],
            vec![call("u1", "key-mine", t, 100, 5, 90), call("u2", "key-mine", t + 1, 200, 15, 180)],
        );
        let conn = f.core.db.get().unwrap();
        let calls = mission_calls(&conn, &m).unwrap();
        let sum = |pick: fn(&CallUsage) -> Option<f64>| -> f64 { calls.iter().map(|c| pick(c).unwrap()).sum() };
        let (a, b) = (sum(|c| c.points_cached_in_full), sum(|c| c.points_cached_free));
        let budget = &mission_budgets(&conn, &m).unwrap()[0];
        assert!(near(a.min(b), budget.attributed_low.unwrap()), "{a} {b} {budget:?}");
        assert!(near(a.max(b), budget.attributed_high.unwrap()));
        assert!((a - b).abs() > 0.1, "the two readings differ here, which is what the test needs");
    }

    #[test]
    fn calls_are_stored_once_and_survive_a_later_log_that_has_lost_them() {
        let f = fixture();
        let m = f.mission();
        keyed_session(&f, "s1", Some(&m), "key-mine");
        let t = at(1).timestamp_micros();
        let first = vec![call("u1", "key-mine", t, 100, 5, 0), call("u2", "key-mine", t + 1, 50, 5, 0)];
        with_calls(&f, &m, 2, 40.0, &[("key-mine", sums(150, 10, 0))], first.clone());
        with_calls(&f, &m, 4, 41.0, &[("key-mine", sums(150, 10, 0))], first);
        // The runtime later compacts its log: u1 is gone. The stored call stays.
        with_calls(&f, &m, 8, 42.0, &[("key-mine", sums(50, 5, 0))], vec![call("u2", "key-mine", t + 1, 50, 5, 0), call("u3", "key-mine", t + 9, 7, 1, 0)]);
        let ids: Vec<String> = mission_calls(&f.core.db.get().unwrap(), &m).unwrap().into_iter().map(|c| c.usage_id).collect();
        assert_eq!(ids, ["u1", "u2", "u3"]);
    }

    #[test]
    fn calls_without_a_known_window_total_have_no_points_but_are_kept() {
        let f = fixture();
        let m = f.mission();
        keyed_session(&f, "s1", Some(&m), "key-mine");
        let t = at(1).timestamp_micros();
        // A reading whose weekly window is missing stores no basis row, so no points can be given.
        let scan = TokenScan { by_session: BTreeMap::new(), calls: vec![call("u1", "key-mine", t, 10, 1, 0)] };
        let no_week = vec![UsageWindow { name: "Window".into(), used_percent: 5.0, resets_at: None }];
        sample_with(&f.core, &m, SampleKind::View, at(2), &|_| Ok(no_week.clone()), &|_, _, _| Some(scan.clone())).unwrap();
        assert!(mission_calls(&f.core.db.get().unwrap(), &m).unwrap().is_empty(), "no week window, no token scan, nothing stored");
    }

    #[test]
    fn the_recent_list_has_each_missions_share_rating_and_only_recent_missions() {
        let f = fixture();
        let measured = f.mission();
        keyed_session(&f, "s1", Some(&measured), "key-mine");
        with_history(&f, &measured, 0, 40.0, RESETS, &[("key-mine", sums(100, 0, 0)), ("key-x", sums(900, 0, 0))]);
        crate::ops::mission::review::mission_rate(
            &f.core,
            runner_core::protocol::MissionRateInput {
                mission_id: measured.clone(),
                verdict: Some(runner_core::protocol::RatingVerdict::Down),
                note: String::new(),
            },
        )
        .unwrap();
        let unmeasured = f.mission();
        let old = f.mission();
        f.core
            .db
            .get()
            .unwrap()
            .execute("UPDATE missions SET started_at = '2020-01-01T00:00:00+00:00' WHERE id = ?1", [&old])
            .unwrap();
        let list = mission_usage_list_at(&f.core, 7, Utc::now()).unwrap();
        let ids: Vec<&str> = list.iter().map(|m| m.mission_id.as_str()).collect();
        assert!(ids.contains(&measured.as_str()) && ids.contains(&unmeasured.as_str()));
        assert!(!ids.contains(&old.as_str()), "a mission from years ago is outside the window");
        let m = list.iter().find(|m| m.mission_id == measured).unwrap();
        assert!(near(m.attributed_low.unwrap(), 4.0), "100 of 1000 tokens, of 40 points: {m:?}");
        assert_eq!(m.verdict, Some(runner_core::protocol::RatingVerdict::Down));
        assert_eq!(m.runtime.as_deref(), Some("muse"));
        let u = list.iter().find(|m| m.mission_id == unmeasured).unwrap();
        assert_eq!((u.attributed_low, u.verdict, u.runtime.clone()), (None, None, None));
    }

    #[test]
    fn a_backfill_measures_missions_that_ran_against_the_open_week_and_never_replaces_a_real_reading() {
        let f = fixture();
        let old_mission = f.mission();
        keyed_session(&f, "s-old", Some(&old_mission), "key-old");
        let empty_mission = f.mission();
        keyed_session(&f, "s-empty", Some(&empty_mission), "key-empty");
        let live_mission = f.mission();
        keyed_session(&f, "s-live", Some(&live_mission), "key-live");
        // The live one already has a real reading for this week.
        with_history(&f, &live_mission, 0, 40.0, RESETS, &[("key-live", sums(50, 0, 0)), ("key-x", sums(950, 0, 0))]);
        let windows = vec![UsageWindow { name: "Week".into(), used_percent: 40.0, resets_at: Some(RESETS.parse().unwrap()) }];
        let by_session: BTreeMap<String, TokenSums> = [
            ("key-old", sums(300, 0, 0)),
            ("key-live", sums(50, 0, 0)),
            ("key-x", sums(650, 0, 0)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        let scan = TokenScan {
            by_session,
            calls: vec![call("u1", "key-old", at(1).timestamp_micros(), 300, 0, 0)],
        };
        let run = || {
            backfill_with(&f.core, 7, at(10), &|_| Ok(windows.clone()), &|_, _, _| Some(scan.clone())).unwrap()
        };
        let first = run();
        assert_eq!((first.measured, first.skipped), (1, 2), "old measured; empty and live skipped");
        let conn = f.core.db.get().unwrap();
        let old = &mission_budgets(&conn, &old_mission).unwrap()[0];
        assert!(near(old.attributed_low.unwrap(), 40.0 * 0.3), "300 of 1000 tokens, of 40 points: {old:?}");
        assert_eq!(mission_calls(&conn, &old_mission).unwrap().len(), 1);
        assert!(mission_budgets(&conn, &empty_mission).unwrap().is_empty(), "no tokens, no measure");
        let live = &mission_budgets(&conn, &live_mission).unwrap()[0];
        assert!(near(live.attributed_low.unwrap(), 40.0 * 0.05), "the real reading stands: {live:?}");
        drop(conn);
        let again = run();
        assert_eq!((again.measured, again.skipped), (0, 3), "running it twice changes nothing");
    }

    #[test]
    fn the_recent_list_reports_when_a_mission_last_made_a_call() {
        let f = fixture();
        let m = f.mission();
        keyed_session(&f, "s1", Some(&m), "key-mine");
        let t = at(1).timestamp_micros();
        with_calls(&f, &m, 2, 40.0, &[("key-mine", sums(30, 0, 0))], vec![call("u1", "key-mine", t, 10, 0, 0), call("u2", "key-mine", t + 5_000_000, 20, 0, 0)]);
        let none = f.mission();
        let list = mission_usage_list_at(&f.core, 7, Utc::now()).unwrap();
        let measured = list.iter().find(|x| x.mission_id == m).unwrap();
        let last: DateTime<Utc> = measured.last_call_at.as_deref().unwrap().parse().unwrap();
        assert_eq!(last.timestamp_micros(), t + 5_000_000, "the later of the two calls");
        assert_eq!(list.iter().find(|x| x.mission_id == none).unwrap().last_call_at, None);
    }

    #[test]
    fn deleting_a_mission_removes_its_readings() {
        let f = fixture();
        let m = f.mission();
        f.session("s1", Some(&m), "muse", SessionStatus::Running);
        keyed_session(&f, "s2", Some(&m), "key-mine");
        f.sample(&m, SampleKind::Start, 0, 40.0, RESETS);
        with_calls(&f, &m, 5, 41.0, &[("key-mine", sums(10, 0, 0))], vec![call("u1", "key-mine", at(1).timestamp_micros(), 10, 0, 0)]);
        let mut conn = f.core.db.get().unwrap();
        let tx = conn.transaction().unwrap();
        crate::ops::mission::delete_rows(&tx, &[m.clone()]).unwrap();
        tx.commit().unwrap();
        let left: i64 = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM mission_budget_sample) + (SELECT COUNT(*) FROM mission_budget_basis) + (SELECT COUNT(*) FROM mission_usage_call)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
    }
}
