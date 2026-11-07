// Muse post-spawn session-key capture.
//
// The muse TUI takes no caller-provided session id (`muse exec` has
// `--session-id`, the interactive CLI does not — probed 2026-11-07),
// so fresh spawns self-assign. Sessions persist under
// `<sessions_root>/YYYY/MM/DD/<uuid>/session.jsonl` (local-date shards)
// whose early records carry `payload.record.workspace_root`, and whose
// first user prompt holds our Runner-owned marker when the spawn had a
// first turn. Resume drives `muse resume <uuid>` via the adapter plan.
//
// Capture mirrors the codex lineage: a short-lived background thread
// polls today's (+ tomorrow's, for midnight spawns) shards; a marker
// match proves ownership, otherwise a spawn-time + workspace_root match
// claims only when unambiguous (single candidate, no sibling row could
// own it). Ambiguity keeps polling until the 30s deadline; failure
// leaves a NULL key and the row spawns fresh on resume — no worse than
// without capture.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::{db::DbPool, session::manager::SessionUpdatedEvent};
use chrono::{DateTime, Datelike, Local, Utc};

use crate::runtimes::RolloutCaptureRequest;

const CAPTURE_TIMEOUT_SECS: u64 = 30;
const POLL_INTERVAL_MS: u64 = 400;
/// Only the head of session.jsonl is ever read: the workspace record and
/// the first user prompt (marker) both land in the first kilobytes, while
/// a long session's log can grow to megabytes.
const LOG_HEAD_BYTES: u64 = 65536;
const LOG_HEAD_LINES: usize = 200;
/// Clock skew allowance between spawn time and session-dir creation.
const STARTED_SKEW_SECS: i64 = 60;

pub fn prompt_marker(session_id: &str) -> String {
    format!("runner-muse-capture-{session_id}")
}

pub(crate) fn mark_prompt(
    session_id: &str,
    first_turn: Option<String>,
) -> (Option<String>, Option<String>) {
    let Some(first_turn) = first_turn else {
        return (None, None);
    };
    let marker = prompt_marker(session_id);
    let marked_first_turn = format!("{first_turn}\n\n{marker}");
    if marked_first_turn.len() > crate::router::runtime::FIRST_TURN_ARGV_MAX_BYTES {
        return (Some(first_turn), None);
    }
    (Some(marked_first_turn), Some(marker))
}

pub(crate) fn spawn_capture(request: RolloutCaptureRequest) {
    std::thread::spawn(move || run(request));
}

fn claimed_sessions() -> &'static Mutex<HashSet<PathBuf>> {
    static CLAIMED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    CLAIMED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn run(request: RolloutCaptureRequest) {
    if !request.sessions_root.is_dir() {
        return;
    }

    // Session dirs are partitioned by *local* date. The user might
    // start muse right before midnight and the session could land in
    // the next day's dir, so probe both.
    let local_started = request.started_at.with_timezone(&Local);
    let candidates = [local_started, local_started + chrono::Duration::days(1)];

    let deadline = Instant::now() + Duration::from_secs(CAPTURE_TIMEOUT_SECS);
    // `done` holds session dirs we've definitively decided about — they
    // matched (we returned) or they're verifiably not ours (cwd
    // mismatch, too old). Dirs with a transient verdict (log missing or
    // mid-write, workspace record not flushed yet) are intentionally NOT
    // inserted, so the next poll re-reads them.
    let mut done: HashSet<PathBuf> = HashSet::new();

    loop {
        let scan = scan_once(
            &request.sessions_root,
            &candidates,
            &request.spawn_cwd,
            request.started_at,
            request.prompt_marker.as_deref(),
            &mut done,
        );

        match scan {
            CaptureScan::Unique(candidate, source) => {
                // A fallback-sourced match additionally needs this row to
                // be the only plausible owner. When a same-cwd sibling row
                // exists, don't guess — but keep polling rather than
                // giving up: once the sibling's watcher captures its
                // session (dropping its NULL key), this row becomes
                // unambiguous on a later poll.
                let row_ok = source != ScanSource::Fallback
                    || fallback_row_is_unambiguous(
                        &request.pool,
                        &request.session_id,
                        &request.expected_row_started_at,
                        &request.spawn_cwd,
                    );
                if row_ok {
                    // Race guard: another watcher may have already
                    // captured this session (sibling chat in the same cwd).
                    // The first to insert into the process-shared set wins.
                    let claimed = claimed_sessions()
                        .lock()
                        .unwrap()
                        .insert(candidate.path.clone());
                    if !claimed {
                        done.insert(candidate.path);
                        continue;
                    }
                    if persist_capture(
                        &request.manager,
                        &request.pool,
                        &request.session_id,
                        &request.expected_row_started_at,
                        &candidate.id,
                    ) {
                        request.events.updated(&SessionUpdatedEvent {
                            session_id: request.session_id.clone(),
                            mission_id: request.mission_id.clone(),
                        });
                    }
                    return;
                }
            }
            CaptureScan::Ambiguous => {
                // Two or more unclaimed candidates. Keep polling instead
                // of failing closed immediately: sibling watchers claim
                // their sessions as their marker proofs land, shrinking
                // the unclaimed set until ours is unique.
            }
            CaptureScan::None => {}
        }
        if Instant::now() > deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}

fn persist_capture(
    manager: &std::sync::Weak<crate::session::manager::SessionManager>,
    pool: &DbPool,
    session_id: &str,
    expected_row_started_at: &str,
    agent_session_key: &str,
) -> bool {
    let Some(manager) = manager.upgrade() else {
        return false;
    };
    manager
        .persist_reported_key(
            session_id,
            crate::session::state::PersistKey {
                key: Some(agent_session_key.into()),
                generation: expected_row_started_at.into(),
                origin: crate::session::state::KeyOrigin::Captured,
            },
            pool,
        )
        .unwrap_or(false)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MatchCandidate {
    pub(crate) path: PathBuf,
    pub(crate) id: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CaptureScan {
    Unique(MatchCandidate, ScanSource),
    Ambiguous,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScanSource {
    Marker,
    Fallback,
}

fn fallback_row_is_unambiguous(
    pool: &DbPool,
    session_id: &str,
    expected_row_started_at: &str,
    spawn_cwd: &str,
) -> bool {
    let Ok(conn) = pool.get() else { return false };
    let current_cwd = crate::repo::session::capture_cwd(
        &conn,
        session_id,
        expected_row_started_at,
        crate::model::Runtime::Muse.key(),
    );
    match current_cwd {
        Ok(Some(cwd)) if cwd != spawn_cwd => return false,
        Ok(_) => {}
        Err(_) => return false,
    }

    let sibling_count = crate::repo::session::count_capture_siblings(
        &conn,
        session_id,
        spawn_cwd,
        crate::model::Runtime::Muse.key(),
    );
    sibling_count.map(|count| count == 0).unwrap_or(false)
}

pub(crate) fn session_dirs_for_dates(
    sessions_root: &Path,
    candidates: &[DateTime<Local>],
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for date in candidates {
        let dir = sessions_root
            .join(format!("{:04}", date.year()))
            .join(format!("{:02}", date.month()))
            .join(format!("{:02}", date.day()));
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        paths.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir()),
        );
    }
    paths
}

/// Read the head of a session log: enough for the workspace record and
/// the first user prompt, bounded so long sessions stay cheap to scan.
fn read_log_head(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.by_ref()
        .take(LOG_HEAD_BYTES)
        .read_to_end(&mut buf)
        .ok()?;
    String::from_utf8(buf).ok()
}

/// The workspace root the CLI recorded at startup, if flushed yet.
fn log_workspace_root(head: &str) -> Option<String> {
    for line in head.lines().take(LOG_HEAD_LINES) {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(root) = value
            .get("payload")
            .and_then(|p| p.get("record"))
            .and_then(|r| r.get("workspace_root"))
            .and_then(|r| r.as_str())
        {
            return Some(root.to_string());
        }
    }
    None
}

fn dir_created_at(path: &Path) -> Option<DateTime<Utc>> {
    let meta = std::fs::metadata(path).ok()?;
    meta.created()
        .or_else(|_| meta.modified())
        .ok()
        .map(DateTime::<Utc>::from)
}

pub(crate) fn scan_once(
    sessions_root: &Path,
    candidates: &[DateTime<Local>],
    spawn_cwd: &str,
    started_at: DateTime<Utc>,
    prompt_marker: Option<&str>,
    done: &mut HashSet<PathBuf>,
) -> CaptureScan {
    let cutoff = started_at - chrono::Duration::seconds(STARTED_SKEW_SECS);
    // Muse records the canonical workspace root (/tmp/x arrives as
    // /private/tmp/x); the spawn cwd may use the uncanonicalized form.
    let canonical_cwd = std::fs::canonicalize(spawn_cwd)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| spawn_cwd.to_string());
    let mut marker_hits: Vec<MatchCandidate> = Vec::new();
    let mut fallback_hits: Vec<MatchCandidate> = Vec::new();

    for dir in session_dirs_for_dates(sessions_root, candidates) {
        if done.contains(&dir) {
            continue;
        }
        let Some(id) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            done.insert(dir);
            continue;
        };
        let log_path = dir.join("session.jsonl");
        let Some(head) = read_log_head(&log_path) else {
            // Log missing or unreadable: transient, re-read next poll.
            continue;
        };
        if let Some(marker) = prompt_marker {
            if head.contains(marker) {
                marker_hits.push(MatchCandidate { path: dir, id });
                continue;
            }
        }
        match log_workspace_root(&head) {
            Some(root) if root == spawn_cwd || root == canonical_cwd => {
                let fresh = dir_created_at(&dir).is_some_and(|t| t >= cutoff);
                if fresh {
                    fallback_hits.push(MatchCandidate { path: dir, id });
                } else {
                    // Same cwd but predates the spawn: not ours.
                    done.insert(dir);
                }
            }
            Some(_) => {
                // Different cwd: verifiably not ours.
                done.insert(dir);
            }
            None => {
                // Workspace record not flushed yet: transient, unless the
                // dir itself predates the spawn (then it never will be).
                let old = dir_created_at(&dir).is_some_and(|t| t < cutoff);
                if old {
                    done.insert(dir);
                }
            }
        }
    }

    let claimed = claimed_sessions().lock().unwrap();
    let unclaimed = |hits: Vec<MatchCandidate>| {
        hits.into_iter()
            .filter(|hit| !claimed.contains(&hit.path))
            .collect::<Vec<_>>()
    };
    let marker_hits = unclaimed(marker_hits);
    if marker_hits.len() == 1 {
        let hit = marker_hits.into_iter().next().unwrap();
        return CaptureScan::Unique(hit, ScanSource::Marker);
    }
    if marker_hits.len() > 1 {
        return CaptureScan::Ambiguous;
    }
    let fallback_hits = unclaimed(fallback_hits);
    if fallback_hits.len() == 1 {
        let hit = fallback_hits.into_iter().next().unwrap();
        return CaptureScan::Unique(hit, ScanSource::Fallback);
    }
    if fallback_hits.len() > 1 {
        return CaptureScan::Ambiguous;
    }
    CaptureScan::None
}
