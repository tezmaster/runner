// A mission's review: the person's rating and what each slot used. The
// daemon stores both and reads neither runtime's session log: usage is
// recorded by whoever harvested it (the crew's `mission-usage` script).

use std::collections::HashSet;

use chrono::Utc;
use rusqlite::TransactionBehavior;

use crate::error::{Error, Result};
use crate::repo;
use crate::AppCore;
use runner_core::protocol::{
    MissionRateInput, MissionRating, MissionReview, MissionUsageRecordInput, SlotUsage,
    RATING_NOTE_MAX_CHARS,
};

const MAX_SLOTS: usize = 64;
const MAX_TEXT_CHARS: usize = 200;

fn clean_note(note: &str) -> Result<String> {
    let note = note.split_whitespace().collect::<Vec<_>>().join(" ");
    if note.chars().count() > RATING_NOTE_MAX_CHARS {
        return Err(Error::msg(format!(
            "a rating note is one line of at most {RATING_NOTE_MAX_CHARS} characters"
        )));
    }
    Ok(note)
}

/// Rate a mission, or clear its rating when no verdict is given. Rating
/// again replaces the earlier rating. An archived or still-running mission
/// can be rated: the person decides when a run is finished.
pub fn mission_rate(core: &AppCore, input: MissionRateInput) -> Result<Option<MissionRating>> {
    let mut conn = core.db.get()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    super::get(&tx, &input.mission_id)?;
    let rating = match input.verdict {
        Some(verdict) => {
            let note = clean_note(&input.note)?;
            let rated_at = Utc::now().to_rfc3339();
            repo::mission_review::set_rating(&tx, &input.mission_id, verdict, &note, &rated_at)?;
            Some(MissionRating {
                verdict,
                note,
                rated_at,
            })
        }
        None => {
            if !input.note.trim().is_empty() {
                return Err(Error::msg("a note needs a verdict; give up or down"));
            }
            repo::mission_review::clear_rating(&tx, &input.mission_id)?;
            None
        }
    };
    tx.commit()?;
    Ok(rating)
}

fn check_text(label: &str, value: &Option<String>) -> Result<()> {
    if value
        .as_deref()
        .is_some_and(|text| text.chars().count() > MAX_TEXT_CHARS)
    {
        return Err(Error::msg(format!(
            "{label} is longer than {MAX_TEXT_CHARS} characters"
        )));
    }
    Ok(())
}

fn check_count(label: &str, value: Option<i64>) -> Result<()> {
    if value.is_some_and(|count| count < 0) {
        return Err(Error::msg(format!("{label} cannot be negative")));
    }
    Ok(())
}

fn checked_slot(mut slot: SlotUsage) -> Result<SlotUsage> {
    slot.handle = slot.handle.trim().to_owned();
    if slot.handle.is_empty() || slot.handle.chars().count() > MAX_TEXT_CHARS {
        return Err(Error::msg("a slot needs a handle of 1 to 200 characters"));
    }
    for (label, value) in [
        ("runtime", &slot.runtime),
        ("model", &slot.model),
        ("effort", &slot.effort),
        ("prompt_hash", &slot.prompt_hash),
        ("not_recorded_reason", &slot.not_recorded_reason),
    ] {
        check_text(label, value)?;
    }
    for (label, value) in [
        ("input_tokens", slot.input_tokens),
        ("output_tokens", slot.output_tokens),
        ("cached_tokens", slot.cached_tokens),
        ("reasoning_tokens", slot.reasoning_tokens),
        ("steps", slot.steps),
    ] {
        check_count(label, value)?;
    }
    if slot
        .wall_minutes
        .is_some_and(|minutes| !minutes.is_finite() || minutes < 0.0)
    {
        return Err(Error::msg("wall_minutes must be a number of at least 0"));
    }
    let no_tokens = slot.input_tokens.is_none()
        && slot.output_tokens.is_none()
        && slot.cached_tokens.is_none()
        && slot.reasoning_tokens.is_none()
        && slot.steps.is_none();
    if no_tokens
        && slot
            .not_recorded_reason
            .as_deref()
            .is_none_or(|reason| reason.trim().is_empty())
    {
        // Never leave a slot looking like zero usage: say it was not recorded.
        slot.not_recorded_reason = Some("not recorded".into());
    }
    Ok(slot)
}

/// Replace the recorded usage of a mission's slots. Whoever harvests a
/// session log calls this at landing; harvesting again replaces the rows.
pub fn mission_usage_record(core: &AppCore, input: MissionUsageRecordInput) -> Result<MissionReview> {
    if input.slots.len() > MAX_SLOTS {
        return Err(Error::msg(format!("at most {MAX_SLOTS} slots")));
    }
    let slots = input
        .slots
        .into_iter()
        .map(checked_slot)
        .collect::<Result<Vec<_>>>()?;
    let mut seen = HashSet::new();
    if let Some(duplicate) = slots.iter().find(|slot| !seen.insert(slot.handle.as_str())) {
        return Err(Error::msg(format!(
            "slot {} appears twice",
            duplicate.handle
        )));
    }
    let mut conn = core.db.get()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    super::get(&tx, &input.mission_id)?;
    repo::mission_review::replace_slot_usage(
        &tx,
        &input.mission_id,
        &slots,
        &Utc::now().to_rfc3339(),
    )?;
    let review = review(&tx, &input.mission_id)?;
    tx.commit()?;
    Ok(review)
}

fn review(conn: &rusqlite::Connection, mission_id: &str) -> Result<MissionReview> {
    Ok(MissionReview {
        mission_id: mission_id.to_owned(),
        rating: repo::mission_review::get_rating(conn, mission_id)?,
        slots: repo::mission_review::list_slot_usage(conn, mission_id)?,
        budget: super::budget::mission_budgets(conn, mission_id)?,
        calls: super::budget::mission_calls(conn, mission_id)?,
    })
}

pub fn mission_review_get(core: &AppCore, mission_id: &str) -> Result<MissionReview> {
    let conn = core.db.get()?;
    super::get(&conn, mission_id)?;
    review(&conn, mission_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::mission::{start, MissionStart};
    use crate::ops::project::ProjectScope;
    use crate::test_support::test_core_in;
    use runner_core::protocol::RatingVerdict;

    struct Fixture {
        _dir: tempfile::TempDir,
        core: AppCore,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let core = test_core_in(dir.path().to_path_buf());
        Fixture { _dir: dir, core }
    }

    impl Fixture {
        fn mission(&self) -> String {
            start(
                &mut self.core.db.get().unwrap(),
                &self.core.app_data_dir,
                MissionStart {
                    crew_id: None,
                    scope: ProjectScope::Root,
                    title: "Reviewed".into(),
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

        fn rate(&self, mission: &str, verdict: Option<RatingVerdict>, note: &str) -> Result<Option<MissionRating>> {
            mission_rate(
                &self.core,
                MissionRateInput {
                    mission_id: mission.into(),
                    verdict,
                    note: note.into(),
                },
            )
        }

        fn record(&self, mission: &str, slots: Vec<SlotUsage>) -> Result<MissionReview> {
            mission_usage_record(
                &self.core,
                MissionUsageRecordInput {
                    mission_id: mission.into(),
                    slots,
                },
            )
        }
    }

    fn slot(handle: &str, input: Option<i64>) -> SlotUsage {
        SlotUsage {
            handle: handle.into(),
            runtime: Some("muse".into()),
            input_tokens: input,
            ..SlotUsage::default()
        }
    }

    #[test]
    fn a_new_mission_has_no_rating_and_no_usage() {
        let f = fixture();
        let m = f.mission();
        let review = mission_review_get(&f.core, &m).unwrap();
        assert_eq!(review.rating, None);
        assert!(review.slots.is_empty());
    }

    #[test]
    fn rating_again_replaces_and_clearing_removes() {
        let f = fixture();
        let m = f.mission();
        f.rate(&m, Some(RatingVerdict::Down), "two escapes").unwrap();
        let second = f.rate(&m, Some(RatingVerdict::Up), "  fine\non retry ").unwrap().unwrap();
        assert_eq!(second.verdict, RatingVerdict::Up);
        assert_eq!(second.note, "fine on retry", "whitespace and newlines collapse to one line");
        assert_eq!(mission_review_get(&f.core, &m).unwrap().rating, Some(second));
        assert_eq!(f.rate(&m, None, "").unwrap(), None);
        assert_eq!(mission_review_get(&f.core, &m).unwrap().rating, None);
    }

    #[test]
    fn a_bad_rating_is_refused_and_changes_nothing() {
        let f = fixture();
        let m = f.mission();
        f.rate(&m, Some(RatingVerdict::Up), "kept").unwrap();
        let long = "x".repeat(RATING_NOTE_MAX_CHARS + 1);
        assert!(f.rate(&m, Some(RatingVerdict::Down), &long).is_err());
        assert!(f.rate(&m, None, "a note without a verdict").is_err());
        assert!(f.rate("no-such-mission", Some(RatingVerdict::Up), "").is_err());
        let kept = mission_review_get(&f.core, &m).unwrap().rating.unwrap();
        assert_eq!((kept.verdict, kept.note.as_str()), (RatingVerdict::Up, "kept"));
    }

    #[test]
    fn recording_usage_replaces_the_earlier_rows() {
        let f = fixture();
        let m = f.mission();
        f.record(&m, vec![slot("coder", Some(10)), slot("reviewer", Some(5))]).unwrap();
        let review = f.record(&m, vec![slot("coder", Some(99))]).unwrap();
        assert_eq!(review.slots.len(), 1);
        assert_eq!(review.slots[0].input_tokens, Some(99));
        assert!(review.slots[0].recorded_at.is_some());
    }

    #[test]
    fn a_slot_with_no_numbers_is_never_shown_as_zero() {
        let f = fixture();
        let m = f.mission();
        let mut unreadable = slot("lead", None);
        unreadable.not_recorded_reason = Some("session not found".into());
        let review = f.record(&m, vec![unreadable, slot("coder", None)]).unwrap();
        let by = |h: &str| review.slots.iter().find(|s| s.handle == h).unwrap().clone();
        assert_eq!(by("lead").input_tokens, None);
        assert_eq!(by("lead").not_recorded_reason.as_deref(), Some("session not found"));
        assert_eq!(by("coder").not_recorded_reason.as_deref(), Some("not recorded"));
    }

    #[test]
    fn bad_usage_is_refused_and_keeps_the_earlier_rows() {
        let f = fixture();
        let m = f.mission();
        f.record(&m, vec![slot("coder", Some(1))]).unwrap();
        assert!(f.record(&m, vec![slot("coder", Some(-1))]).is_err());
        assert!(f.record(&m, vec![slot("a", Some(1)), slot("a", Some(2))]).is_err());
        assert!(f.record(&m, vec![slot(" ", Some(1))]).is_err());
        let mut nan = slot("coder", Some(1));
        nan.wall_minutes = Some(f64::NAN);
        assert!(f.record(&m, vec![nan]).is_err());
        assert!(f.record("no-such-mission", vec![]).is_err());
        let kept = mission_review_get(&f.core, &m).unwrap().slots;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].input_tokens, Some(1));
    }

    #[test]
    fn deleting_a_mission_deletes_its_review() {
        let f = fixture();
        let m = f.mission();
        f.rate(&m, Some(RatingVerdict::Up), "").unwrap();
        f.record(&m, vec![slot("coder", Some(1))]).unwrap();
        let mut conn = f.core.db.get().unwrap();
        let tx = conn.transaction().unwrap();
        crate::ops::mission::delete_rows(&tx, &[m.clone()]).unwrap();
        tx.commit().unwrap();
        let left: i64 = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM mission_rating) + (SELECT COUNT(*) FROM mission_slot_usage)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
    }
}
