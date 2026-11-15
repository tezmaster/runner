//! A person's rating of a mission and what each slot used, so finished runs
//! can be reviewed afterwards. The daemon stores both; it does not read any
//! runtime's session log, so usage arrives from whoever harvested it.

use serde::{Deserialize, Serialize};

/// Longest note a rating keeps, in characters. A note says what went well or
/// badly in the work; it is a line, not a report.
pub const RATING_NOTE_MAX_CHARS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RatingVerdict {
    Up,
    Down,
}

impl RatingVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionRating {
    pub verdict: RatingVerdict,
    pub note: String,
    pub rated_at: String,
}

/// Rate a mission, or clear its rating when `verdict` is `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionRateInput {
    pub mission_id: String,
    #[serde(default)]
    pub verdict: Option<RatingVerdict>,
    #[serde(default)]
    pub note: String,
}

/// One slot's usage. Token fields are `None` when the usage could not be
/// read, and `not_recorded_reason` says why: `None` is never zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SlotUsage {
    pub handle: String,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    /// A short hash of the role prompt the slot ran with.
    #[serde(default)]
    pub prompt_hash: Option<String>,
    #[serde(default)]
    pub input_tokens: Option<i64>,
    #[serde(default)]
    pub output_tokens: Option<i64>,
    #[serde(default)]
    pub cached_tokens: Option<i64>,
    #[serde(default)]
    pub reasoning_tokens: Option<i64>,
    #[serde(default)]
    pub steps: Option<i64>,
    /// Whether `input_tokens` already counts the cached ones. Runtimes differ,
    /// so counts are never summed across runtimes; `None` means unknown.
    #[serde(default)]
    pub input_includes_cached: Option<bool>,
    #[serde(default)]
    pub wall_minutes: Option<f64>,
    #[serde(default)]
    pub not_recorded_reason: Option<String>,
    /// Set by the daemon when it stores the row; ignored on input.
    #[serde(default)]
    pub recorded_at: Option<String>,
}

/// Replace a mission's recorded slot usage with `slots`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionUsageRecordInput {
    pub mission_id: String,
    pub slots: Vec<SlotUsage>,
}

/// Whether a mission's share of a plan allowance could be worked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetState {
    /// Two or more readings, no reset between them: `used_points` is set.
    Measured,
    /// Fewer than two readings so far.
    NotYetMeasured,
    /// The allowance reset between the first and the latest reading.
    SpannedReset,
}

/// A mission's share of one runtime's plan allowance window, worked out from
/// readings of the whole account. Percentages are whole numbers, so
/// `used_points` is good to about one point either way. Other use of the
/// account counts too: `other_sessions` is how many of Runner's own sessions
/// on that runtime overlapped, and use outside Runner is not visible at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionBudget {
    pub runtime: String,
    pub window: String,
    pub state: BudgetState,
    pub first_percent: f64,
    pub latest_percent: f64,
    /// Percentage points of the window used between the first and latest reading.
    pub used_points: Option<f64>,
    pub first_read_at: String,
    pub latest_read_at: String,
    pub readings: u32,
    pub other_sessions: u32,
    /// The mission's points of the window, split by tokens: the least and the
    /// most, depending on whether cached input counts against the allowance
    /// (not known). `None` until a token reading exists for the window.
    #[serde(default)]
    pub attributed_low: Option<f64>,
    #[serde(default)]
    pub attributed_high: Option<f64>,
    /// Points either way from the whole-percent rounding of the window's total.
    #[serde(default)]
    pub rounding_margin: Option<f64>,
    /// How much of the window the whole account had used at the latest reading.
    #[serde(default)]
    pub window_points: Option<f64>,
    /// The mission's share of all tokens used on this machine in the window,
    /// least and most, as fractions of 1.
    #[serde(default)]
    pub share_low: Option<f64>,
    #[serde(default)]
    pub share_high: Option<f64>,
}

/// One model call of a mission, with the points of the allowance it used,
/// split by tokens the same way as the mission's total.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallUsage {
    pub usage_id: String,
    pub runtime: String,
    /// Ties the call to a session, and so to a slot.
    pub session_key: String,
    pub at: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    /// Points of the weekly allowance this call used, worked out two ways
    /// because whether cached input counts against an allowance is not known:
    /// with cached input counted in full, and with it counted as free. Each is
    /// kept apart so calls can be added up under one reading and then compared.
    /// `None` until the window's total and the machine's tokens are known.
    #[serde(default)]
    pub points_cached_in_full: Option<f64>,
    #[serde(default)]
    pub points_cached_free: Option<f64>,
}

/// What measuring missions that already ran did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillSummary {
    pub measured: u32,
    pub skipped: u32,
}

/// One mission in the recent-usage list: what it used of a weekly allowance
/// and how the person rated it. The figures are those of `MissionBudget`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionUsageSummary {
    pub mission_id: String,
    pub title: String,
    pub status: String,
    pub started_at: String,
    /// When the mission was stopped, if it has been.
    #[serde(default)]
    pub stopped_at: Option<String>,
    /// When its allowance was last read.
    #[serde(default)]
    pub last_read_at: Option<String>,
    /// When its last model call was made: the latest sign it was active, for
    /// a mission that was never stopped.
    #[serde(default)]
    pub last_call_at: Option<String>,
    /// The runtime whose weekly allowance the figures are for.
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub attributed_low: Option<f64>,
    #[serde(default)]
    pub attributed_high: Option<f64>,
    /// The weekly window's whole-percent total at the latest reading.
    #[serde(default)]
    pub window_points: Option<f64>,
    #[serde(default)]
    pub verdict: Option<RatingVerdict>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionReview {
    pub mission_id: String,
    pub rating: Option<MissionRating>,
    pub slots: Vec<SlotUsage>,
    #[serde(default)]
    pub budget: Vec<MissionBudget>,
    #[serde(default)]
    pub calls: Vec<CallUsage>,
}
