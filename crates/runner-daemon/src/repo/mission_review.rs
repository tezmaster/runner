// `mission_rating` and `mission_slot_usage` tables: a person's verdict on a
// mission and what each slot used. Transaction boundaries belong to callers.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Error, Result};
use runner_core::protocol::{MissionRating, RatingVerdict, SlotUsage};

pub fn set_rating(
    conn: &Connection,
    mission_id: &str,
    verdict: RatingVerdict,
    note: &str,
    rated_at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO mission_rating (mission_id, verdict, note, rated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(mission_id) DO UPDATE SET
             verdict = excluded.verdict, note = excluded.note, rated_at = excluded.rated_at",
        params![mission_id, verdict.as_str(), note, rated_at],
    )?;
    Ok(())
}

pub fn clear_rating(conn: &Connection, mission_id: &str) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM mission_rating WHERE mission_id = ?1",
        params![mission_id],
    )? > 0)
}

pub fn get_rating(conn: &Connection, mission_id: &str) -> Result<Option<MissionRating>> {
    let row = conn
        .query_row(
            "SELECT verdict, note, rated_at FROM mission_rating WHERE mission_id = ?1",
            params![mission_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    row.map(|(verdict, note, rated_at)| {
        let verdict = RatingVerdict::parse(&verdict)
            .ok_or_else(|| Error::msg(format!("stored rating verdict {verdict:?} is invalid")))?;
        Ok(MissionRating {
            verdict,
            note,
            rated_at,
        })
    })
    .transpose()
}

/// Delete the rating and recorded usage of `mission_ids`, ahead of deleting
/// the missions themselves (the foreign keys are RESTRICT).
pub fn delete_for_missions(conn: &Connection, mission_ids: &[String]) -> Result<()> {
    if mission_ids.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; mission_ids.len()].join(", ");
    for table in [
        "mission_rating",
        "mission_slot_usage",
        "mission_budget_sample",
        "mission_budget_basis",
        "mission_usage_call",
    ] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE mission_id IN ({placeholders})"),
            rusqlite::params_from_iter(mission_ids),
        )?;
    }
    Ok(())
}

/// Replace every recorded slot of `mission_id` with `slots`.
pub fn replace_slot_usage(
    conn: &Connection,
    mission_id: &str,
    slots: &[SlotUsage],
    recorded_at: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM mission_slot_usage WHERE mission_id = ?1",
        params![mission_id],
    )?;
    let mut insert = conn.prepare(
        "INSERT INTO mission_slot_usage (
             mission_id, handle, runtime, model, effort, prompt_hash,
             input_tokens, output_tokens, cached_tokens, reasoning_tokens, steps,
             input_includes_cached, wall_minutes, not_recorded_reason, recorded_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?;
    for slot in slots {
        insert.execute(params![
            mission_id,
            slot.handle,
            slot.runtime,
            slot.model,
            slot.effort,
            slot.prompt_hash,
            slot.input_tokens,
            slot.output_tokens,
            slot.cached_tokens,
            slot.reasoning_tokens,
            slot.steps,
            slot.input_includes_cached,
            slot.wall_minutes,
            slot.not_recorded_reason,
            recorded_at,
        ])?;
    }
    Ok(())
}

pub fn list_slot_usage(conn: &Connection, mission_id: &str) -> Result<Vec<SlotUsage>> {
    let mut stmt = conn.prepare(
        "SELECT handle, runtime, model, effort, prompt_hash,
                input_tokens, output_tokens, cached_tokens, reasoning_tokens, steps,
                input_includes_cached, wall_minutes, not_recorded_reason, recorded_at
         FROM mission_slot_usage WHERE mission_id = ?1 ORDER BY handle",
    )?;
    let rows = stmt.query_map(params![mission_id], |row| {
        Ok(SlotUsage {
            handle: row.get(0)?,
            runtime: row.get(1)?,
            model: row.get(2)?,
            effort: row.get(3)?,
            prompt_hash: row.get(4)?,
            input_tokens: row.get(5)?,
            output_tokens: row.get(6)?,
            cached_tokens: row.get(7)?,
            reasoning_tokens: row.get(8)?,
            steps: row.get(9)?,
            input_includes_cached: row.get(10)?,
            wall_minutes: row.get(11)?,
            not_recorded_reason: row.get(12)?,
            recorded_at: row.get(13)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// One reading of one window of a runtime's allowance.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetSampleRow {
    pub runtime: String,
    pub window_name: String,
    pub used_percent: f64,
    pub resets_at: Option<String>,
    pub read_at: String,
    pub kind: String,
}

pub fn insert_budget_sample(
    conn: &Connection,
    mission_id: &str,
    row: &BudgetSampleRow,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO mission_budget_sample
             (mission_id, runtime, window_name, used_percent, resets_at, read_at, kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            mission_id,
            row.runtime,
            row.window_name,
            row.used_percent,
            row.resets_at,
            row.read_at,
            row.kind
        ],
    )?;
    Ok(())
}

/// Every reading for a mission, oldest first.
pub fn list_budget_samples(conn: &Connection, mission_id: &str) -> Result<Vec<BudgetSampleRow>> {
    let mut stmt = conn.prepare(
        "SELECT runtime, window_name, used_percent, resets_at, read_at, kind
         FROM mission_budget_sample WHERE mission_id = ?1 ORDER BY read_at, runtime, window_name",
    )?;
    let rows = stmt.query_map(params![mission_id], |row| {
        Ok(BudgetSampleRow {
            runtime: row.get(0)?,
            window_name: row.get(1)?,
            used_percent: row.get(2)?,
            resets_at: row.get(3)?,
            read_at: row.get(4)?,
            kind: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// The agent sessions that are not part of `mission_id`, on `runtime`: their
/// start, their stop (None while running) and whether they are still live.
pub fn other_sessions_on_runtime(
    conn: &Connection,
    mission_id: &str,
    runtime: &str,
) -> Result<Vec<(Option<String>, Option<String>, bool)>> {
    let mut stmt = conn.prepare(
        "SELECT started_at, stopped_at, status = 'running'
         FROM sessions
         WHERE agent_runtime = ?1 AND (mission_id IS NULL OR mission_id != ?2)",
    )?;
    let rows = stmt.query_map(params![runtime, mission_id], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// The runtimes of a mission's sessions, with each runtime named once.
pub fn mission_runtimes(conn: &Connection, mission_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT agent_runtime FROM sessions
         WHERE mission_id = ?1 AND agent_runtime IS NOT NULL ORDER BY agent_runtime",
    )?;
    let rows = stmt.query_map(params![mission_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Whether the mission has a session running now.
pub fn has_live_session(conn: &Connection, mission_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE mission_id = ?1 AND status = 'running')",
        params![mission_id],
        |row| row.get::<_, bool>(0),
    )?)
}

/// A mission's and the machine's token totals at a reading, cumulative from
/// the start of the allowance window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cached: i64,
    pub reasoning: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetBasisRow {
    pub runtime: String,
    pub read_at: String,
    pub week_start: String,
    pub mission: Tokens,
    pub machine: Tokens,
}

pub fn insert_budget_basis(conn: &Connection, mission_id: &str, row: &BudgetBasisRow) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO mission_budget_basis (
             mission_id, runtime, read_at, week_start,
             mission_input, mission_output, mission_cached, mission_reasoning,
             machine_input, machine_output, machine_cached, machine_reasoning
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            mission_id,
            row.runtime,
            row.read_at,
            row.week_start,
            row.mission.input,
            row.mission.output,
            row.mission.cached,
            row.mission.reasoning,
            row.machine.input,
            row.machine.output,
            row.machine.cached,
            row.machine.reasoning,
        ],
    )?;
    Ok(())
}

pub fn list_budget_basis(conn: &Connection, mission_id: &str) -> Result<Vec<BudgetBasisRow>> {
    let mut stmt = conn.prepare(
        "SELECT runtime, read_at, week_start,
                mission_input, mission_output, mission_cached, mission_reasoning,
                machine_input, machine_output, machine_cached, machine_reasoning
         FROM mission_budget_basis WHERE mission_id = ?1 ORDER BY read_at, runtime",
    )?;
    let rows = stmt.query_map(params![mission_id], |row| {
        Ok(BudgetBasisRow {
            runtime: row.get(0)?,
            read_at: row.get(1)?,
            week_start: row.get(2)?,
            mission: Tokens {
                input: row.get(3)?,
                output: row.get(4)?,
                cached: row.get(5)?,
                reasoning: row.get(6)?,
            },
            machine: Tokens {
                input: row.get(7)?,
                output: row.get(8)?,
                cached: row.get(9)?,
                reasoning: row.get(10)?,
            },
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// The agent session keys of a mission's sessions, which tie them to a
/// runtime's own logs.
pub fn mission_session_keys(conn: &Connection, mission_id: &str, runtime: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT agent_session_key FROM sessions
         WHERE mission_id = ?1 AND agent_runtime = ?2 AND agent_session_key IS NOT NULL",
    )?;
    let rows = stmt.query_map(params![mission_id, runtime], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// One model call of a mission's sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRow {
    pub usage_id: String,
    pub runtime: String,
    pub session_key: String,
    pub at: String,
    pub tokens: Tokens,
}

/// Stores calls; a call already stored is left as it is.
pub fn insert_calls(conn: &Connection, mission_id: &str, calls: &[CallRow]) -> Result<()> {
    let mut insert = conn.prepare(
        "INSERT OR IGNORE INTO mission_usage_call (
             mission_id, usage_id, runtime, session_key, at,
             input_tokens, output_tokens, cached_tokens, reasoning_tokens
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for call in calls {
        insert.execute(params![
            mission_id,
            call.usage_id,
            call.runtime,
            call.session_key,
            call.at,
            call.tokens.input,
            call.tokens.output,
            call.tokens.cached,
            call.tokens.reasoning,
        ])?;
    }
    Ok(())
}

pub fn list_calls(conn: &Connection, mission_id: &str) -> Result<Vec<CallRow>> {
    let mut stmt = conn.prepare(
        "SELECT usage_id, runtime, session_key, at,
                input_tokens, output_tokens, cached_tokens, reasoning_tokens
         FROM mission_usage_call WHERE mission_id = ?1 ORDER BY at, usage_id",
    )?;
    let rows = stmt.query_map(params![mission_id], |row| {
        Ok(CallRow {
            usage_id: row.get(0)?,
            runtime: row.get(1)?,
            session_key: row.get(2)?,
            at: row.get(3)?,
            tokens: Tokens {
                input: row.get(4)?,
                output: row.get(5)?,
                cached: row.get(6)?,
                reasoning: row.get(7)?,
            },
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
