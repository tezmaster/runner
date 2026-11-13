// What every Muse session on this machine has used since a time, read from
// Muse's own session logs: one `goal_usage_attribution` record per model call,
// with a microsecond timestamp. The store is date-sharded by when a session
// began, but a session keeps writing for days, so every shard is walked and a
// file is skipped only when it was last written before `since`. Sessions
// started outside Runner are included, which is what lets a mission's share be
// measured against everything that drew on the allowance. Sub-agent logs sit
// under their parent session and count toward it.
//
// Token counts and times only: no prompt or reply text is read into memory
// beyond the line being scanned.

use super::super::{CallRecord, TokenScan, TokenSums};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

fn sessions_root() -> Option<PathBuf> {
    // Muse honours XDG_DATA_HOME; as in key capture, the default location is pinned.
    runner_core::app_paths::home_dir().map(|home| {
        home.join(".local")
            .join("share")
            .join("muse")
            .join("sessions")
    })
}

pub(crate) fn read_since(
    since: DateTime<Utc>,
    keys: &HashSet<String>,
) -> Result<TokenScan, String> {
    let root = sessions_root().ok_or("no home directory")?;
    read_tokens(&root, since, keys)
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default()
}

/// Every `session.jsonl` under a root laid out as `YYYY/MM/DD/<key>/`, with
/// sub-agent logs at `<key>/subagent/<id>/session.jsonl`. Each is paired with
/// its session key.
fn log_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for year in subdirs(root) {
        for month in subdirs(&year) {
            for day in subdirs(&month) {
                for session in subdirs(&day) {
                    let Some(key) = session.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    out.push((key.to_owned(), session.join("session.jsonl")));
                    for sub in subdirs(&session.join("subagent")) {
                        out.push((key.to_owned(), sub.join("session.jsonl")));
                    }
                }
            }
        }
    }
    out
}

pub(crate) fn read_tokens(
    root: &Path,
    since: DateTime<Utc>,
    keys: &HashSet<String>,
) -> Result<TokenScan, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }
    let since_us = since.timestamp_micros();
    let since_time: std::time::SystemTime = since.into();
    let mut by_session: BTreeMap<String, TokenSums> = BTreeMap::new();
    let mut calls: Vec<CallRecord> = Vec::new();
    for (key, path) in log_files(root) {
        let fresh = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| modified >= since_time);
        if !fresh {
            continue;
        }
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if !line.contains("goal_usage_attribution") {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let event = value.pointer("/payload/event");
            if event.and_then(|e| e.get("kind")).and_then(Value::as_str)
                != Some("goal_usage_attribution")
            {
                continue;
            }
            let Some(at_us) = value
                .get("recorded_at")
                .and_then(Value::as_i64)
                .filter(|at| *at >= since_us)
            else {
                continue;
            };
            let Some(quantity) = event.and_then(|e| e.pointer("/record/quantity")) else {
                continue;
            };
            let count = |name: &str| {
                quantity
                    .get(name)
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .max(0)
            };
            let tokens = TokenSums {
                input: count("input_tokens"),
                output: count("output_tokens"),
                cached: count("cached_tokens"),
                reasoning: count("reasoning_tokens"),
            };
            by_session.entry(key.clone()).or_default().add(&tokens);
            if keys.contains(&key) {
                let usage_id = event
                    .and_then(|e| e.pointer("/record/usage_id"))
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("{key}@{at_us}"), str::to_owned);
                calls.push(CallRecord {
                    usage_id,
                    session_key: key.clone(),
                    at_us,
                    tokens,
                });
            }
        }
    }
    calls.sort_by(|a, b| (a.at_us, &a.usage_id).cmp(&(b.at_us, &b.usage_id)));
    Ok(TokenScan { by_session, calls })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn record(at_us: i64, input: i64, output: i64, cached: i64, reasoning: i64) -> String {
        format!(
            r#"{{"recorded_at":{at_us},"payload":{{"event":{{"kind":"goal_usage_attribution","record":{{"quantity":{{"unit":"tokens","input_tokens":{input},"output_tokens":{output},"cached_tokens":{cached},"reasoning_tokens":{reasoning},"main_llm_steps":1}}}}}}}}}}"#
        )
    }

    fn write(root: &Path, shard: &str, key: &str, sub: Option<&str>, lines: &[String]) {
        let dir = match sub {
            Some(sub) => root.join(shard).join(key).join("subagent").join(sub),
            None => root.join(shard).join(key),
        };
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("session.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    fn at(rfc: &str) -> DateTime<Utc> {
        rfc.parse().unwrap()
    }

    #[test]
    fn sums_per_session_with_sub_agents_counted_to_their_parent() {
        let root = tempfile::tempdir().unwrap();
        let t = at("2026-11-12T10:00:00Z").timestamp_micros();
        write(root.path(), "2026/11/12", "aaa", None, &[record(t, 100, 10, 60, 5), record(t + 1, 50, 5, 0, 0)]);
        write(root.path(), "2026/11/12", "aaa", Some("sub1"), &[record(t + 2, 7, 1, 2, 0)]);
        write(root.path(), "2026/11/11", "bbb", None, &[record(t, 1_000, 100, 900, 0)]);
        let sums = read_tokens(root.path(), at("2026-11-09T00:00:00Z"), &HashSet::new()).unwrap().by_session;
        assert_eq!(sums["aaa"], TokenSums { input: 157, output: 16, cached: 62, reasoning: 5 });
        assert_eq!(sums["bbb"].input, 1_000);
        assert_eq!(sums.len(), 2);
    }

    #[test]
    fn calls_before_the_start_and_unrelated_lines_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        let before = at("2026-11-08T23:59:59Z").timestamp_micros();
        let after = at("2026-11-09T00:00:01Z").timestamp_micros();
        write(
            root.path(),
            "2026/11/08",
            "ccc",
            None,
            &[
                record(before, 999, 999, 0, 0),
                record(after, 4, 2, 1, 0),
                r#"{"payload":{"event":{"kind":"other","record":{"quantity":{"input_tokens":5000}}}}}"#.to_owned(),
                "not json but mentions goal_usage_attribution".to_owned(),
                r#"{"recorded_at":1,"payload":{"event":{"kind":"goal_usage_attribution"}}}"#.to_owned(),
            ],
        );
        let sums = read_tokens(root.path(), at("2026-11-09T00:00:00Z"), &HashSet::new()).unwrap().by_session;
        assert_eq!(sums["ccc"], TokenSums { input: 4, output: 2, cached: 1, reasoning: 0 });
    }

    #[test]
    fn a_log_last_written_before_the_start_is_skipped_unread() {
        let root = tempfile::tempdir().unwrap();
        let t = at("2026-11-12T10:00:00Z").timestamp_micros();
        write(root.path(), "2026/11/12", "old", None, &[record(t, 10, 1, 0, 0)]);
        let sums = read_tokens(root.path(), Utc::now() + chrono::Duration::days(1), &HashSet::new()).unwrap().by_session;
        assert!(sums.is_empty(), "the file's own modified time is before the cut-off");
    }

    #[test]
    fn calls_are_returned_for_the_asked_sessions_only_oldest_first_with_their_ids() {
        let root = tempfile::tempdir().unwrap();
        let t = at("2026-11-12T10:00:00Z").timestamp_micros();
        let with_id = |id: &str, at_us: i64, input: i64| {
            record(at_us, input, 1, 0, 0).replace(
                r#""record":{"quantity""#,
                &format!(r#""record":{{"usage_id":"{id}","quantity""#),
            )
        };
        write(root.path(), "2026/11/12", "mine", None, &[with_id("u2", t + 20, 50), with_id("u1", t + 10, 30)]);
        write(root.path(), "2026/11/12", "mine", Some("sub"), &[with_id("u3", t + 15, 5)]);
        write(root.path(), "2026/11/12", "theirs", None, &[with_id("u9", t + 5, 999)]);
        let keys: HashSet<String> = ["mine".to_owned()].into();
        let scan = read_tokens(root.path(), at("2026-11-09T00:00:00Z"), &keys).unwrap();
        assert_eq!(
            scan.calls.iter().map(|c| (c.usage_id.as_str(), c.tokens.input)).collect::<Vec<_>>(),
            [("u1", 30), ("u3", 5), ("u2", 50)],
            "sub-agent calls count to their session; another session's calls are not returned"
        );
        assert_eq!(scan.by_session["theirs"].input, 999, "but its tokens are still in the machine totals");
    }

    #[test]
    fn a_missing_store_is_an_error_not_zero() {
        assert!(read_tokens(Path::new("/no/such/muse/sessions"), at("2026-11-09T00:00:00Z"), &HashSet::new()).is_err());
    }

    #[test]
    fn token_bases_differ_by_how_cached_input_is_counted() {
        let sums = TokenSums { input: 100, output: 10, cached: 80, reasoning: 3 };
        assert_eq!(sums.total_basis(), 110);
        assert_eq!(sums.uncached_basis(), 30);
        let odd = TokenSums { input: 5, output: 1, cached: 9, reasoning: 0 };
        assert_eq!(odd.uncached_basis(), 1, "cached above input never goes negative");
    }
}

#[cfg(test)]
mod real_store_check {
    use super::*;

    /// Prints this machine's totals since a time given in RUNNER_HISTORY_SINCE
    /// (RFC 3339). Ignored by default: it reads the real Muse store.
    #[test]
    #[ignore]
    fn print_real_totals() {
        let since: DateTime<Utc> = std::env::var("RUNNER_HISTORY_SINCE")
            .expect("set RUNNER_HISTORY_SINCE")
            .parse()
            .unwrap();
        let started = std::time::Instant::now();
        let sums = read_since(since, &HashSet::new()).unwrap().by_session;
        let mut all = TokenSums::default();
        for sums in sums.values() {
            all.add(sums);
        }
        println!(
            "REAL sessions={} input={} output={} cached={} reasoning={} total_basis={} uncached_basis={} took_ms={}",
            sums.len(), all.input, all.output, all.cached, all.reasoning,
            all.total_basis(), all.uncached_basis(), started.elapsed().as_millis()
        );
    }
}
