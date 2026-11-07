use super::*;
use crate::runtimes::test_support::*;

#[test]
fn muse_fresh_returns_empty_plan() {
    let plan = resume_plan(Runtime::Muse.key(), None);
    assert!(plan.args.is_empty());
    assert!(plan.assigned_key.is_none());
    assert!(!plan.resuming);
}

#[test]
fn muse_resume_uses_subcommand_prefix() {
    let prior = "019fa1b9-a133-7841-b4dd-730d376ab1d1".to_string();
    let plan = resume_plan(Runtime::Muse.key(), Some(&prior));
    assert!(plan.resuming);
    assert!(plan.prepend, "muse resume is a subcommand, must prepend");
    assert_eq!(plan.args, vec!["resume", &prior]);
    assert_eq!(plan.assigned_key.as_deref(), Some(prior.as_str()));
}

#[test]
fn muse_emits_model_and_reasoning_effort_override() {
    let args = model_effort_args(Runtime::Muse.key(), Some("muse-model"), Some("High"));
    assert_eq!(
        args,
        vec![
            "--model".to_string(),
            "muse-model".to_string(),
            "--reasoning-effort".to_string(),
            "high".to_string(),
        ],
    );
}

#[test]
fn muse_empty_model_and_effort_pass_no_flags() {
    let args = model_effort_args(Runtime::Muse.key(), None, None);
    assert!(args.is_empty());
    let args = model_effort_args(Runtime::Muse.key(), Some("  "), Some(""));
    assert!(args.is_empty());
}

#[test]
fn apply_permission_mode_muse_default_strips_approval_flags() {
    let user = vec![
        "--debug".to_string(),
        "--approval-mode".to_string(),
        "never".to_string(),
        "--yolo".to_string(),
    ];
    let cleaned = apply_permission_mode(Runtime::Muse.key(), &user, PermissionMode::Default);
    assert_eq!(cleaned, vec!["--debug".to_string()]);
    assert_eq!(
        infer_permission_mode(Runtime::Muse.key(), &cleaned),
        PermissionMode::Default
    );
}

#[test]
fn infer_permission_mode_muse_bypass_forms() {
    for argv in [
        vec!["--yolo".to_string()],
        vec!["--disable-approval".to_string()],
        vec!["--approval-mode".to_string(), "never".to_string()],
        vec![
            "--approval-mode=never".to_string(),
            "--workspace".to_string(),
            "/tmp/proj".to_string(),
        ],
    ] {
        assert_eq!(
            infer_permission_mode(Runtime::Muse.key(), &argv),
            PermissionMode::Bypass,
            "argv: {argv:?}"
        );
    }
    assert_eq!(
        infer_permission_mode(
            Runtime::Muse.key(),
            &["--approval-mode".to_string(), "on-request".to_string()]
        ),
        PermissionMode::Default
    );
}

#[test]
fn mission_permission_args_muse_use_yolo() {
    assert_eq!(
        mission_permission_args(Runtime::Muse.key()),
        vec!["--yolo".to_string()]
    );
    // Mission args replace, not stack with, stale role permission flags.
    let applied = apply_mission_permissions(
        Runtime::Muse.key(),
        &[
            "--workspace".to_string(),
            "/tmp/proj".to_string(),
            "--approval-mode".to_string(),
            "on-request".to_string(),
        ],
    );
    assert_eq!(
        applied,
        vec![
            "--workspace".to_string(),
            "/tmp/proj".to_string(),
            "--yolo".to_string()
        ]
    );
}

#[test]
fn muse_key_capture_points_at_muse_sessions_root() {
    let KeyCapture::RolloutScan { sessions_root } = Muse.key_capture() else {
        panic!("muse must capture via session-store scan");
    };
    let root = sessions_root.expect("sessions root");
    assert!(root.ends_with(".local/share/muse/sessions"), "{root:?}");
}

#[test]
fn mark_prompt_without_first_turn_marks_nothing() {
    assert_eq!(capture::mark_prompt("row-1", None), (None, None));
}

#[test]
fn mark_prompt_appends_unique_marker() {
    let (turn, marker) = capture::mark_prompt("row-1", Some("do the thing".to_string()));
    let marker = marker.expect("marker");
    assert!(marker.contains("row-1"));
    let turn = turn.expect("marked turn");
    assert!(turn.starts_with("do the thing\n\n"));
    assert!(turn.contains(&marker));
    let (_, other) = capture::mark_prompt("row-2", Some("do the thing".to_string()));
    assert_ne!(marker, other.expect("marker"));
}

#[test]
fn mark_prompt_oversize_body_keeps_prompt_unmarked() {
    let big = "x".repeat(crate::router::runtime::FIRST_TURN_ARGV_MAX_BYTES + 1);
    let (turn, marker) = capture::mark_prompt("row-1", Some(big.clone()));
    assert_eq!(turn.as_deref(), Some(big.as_str()));
    assert_eq!(marker, None);
}

mod capture_scan {
    use super::capture::{scan_once, CaptureScan, ScanSource};
    use chrono::{Datelike, Local, Utc};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static FIXTURE_SEQ: AtomicU64 = AtomicU64::new(0);

    fn fixture_root() -> PathBuf {
        let seq = FIXTURE_SEQ.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("runner-muse-capture-{}-{seq}", std::process::id()))
    }

    fn write_session(root: &Path, id: &str, workspace_root: &str, prompt: &str) -> PathBuf {
        let now = Local::now();
        let dir = root
            .join(format!("{:04}", now.year()))
            .join(format!("{:02}", now.month()))
            .join(format!("{:02}", now.day()))
            .join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let log = format!(
            "{{\"payload\":{{\"record\":{{\"workspace_root\":\"{workspace_root}\"}}}}}}\n\
             {{\"payload\":{{\"event\":{{\"kind\":\"x\"}},\"prompt\":\"{prompt}\"}}}}\n"
        );
        std::fs::write(dir.join("session.jsonl"), log).unwrap();
        dir
    }

    fn scan(root: &Path, spawn_cwd: &str, marker: Option<&str>) -> (CaptureScan, HashSet<PathBuf>) {
        let now = Local::now();
        let candidates = vec![now, now + chrono::Duration::days(1)];
        let mut done = HashSet::new();
        let scan = scan_once(root, &candidates, spawn_cwd, Utc::now(), marker, &mut done);
        (scan, done)
    }

    #[test]
    fn marker_proves_ownership_among_same_cwd_siblings() {
        let root = fixture_root();
        let marked = write_session(
            &root,
            "owned-session",
            "/tmp/proj",
            "do the thing\n\nrunner-muse-capture-row-1",
        );
        write_session(&root, "other-session", "/tmp/proj", "unrelated work");
        let (scan, _) = scan(&root, "/tmp/proj", Some("runner-muse-capture-row-1"));
        match scan {
            CaptureScan::Unique(hit, ScanSource::Marker) => assert_eq!(hit.path, marked),
            other => panic!("expected unique marker hit, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fallback_claims_single_fresh_match() {
        let root = fixture_root();
        let dir = write_session(&root, "only-session", "/tmp/proj", "no marker here");
        let (scan, _) = scan(&root, "/tmp/proj", None);
        match scan {
            CaptureScan::Unique(hit, ScanSource::Fallback) => assert_eq!(hit.path, dir),
            other => panic!("expected unique fallback hit, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fallback_two_matches_stays_ambiguous() {
        let root = fixture_root();
        write_session(&root, "session-a", "/tmp/proj", "no marker");
        write_session(&root, "session-b", "/tmp/proj", "no marker");
        let (scan, _) = scan(&root, "/tmp/proj", None);
        assert_eq!(scan, CaptureScan::Ambiguous);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn other_cwd_is_decided_not_ours() {
        let root = fixture_root();
        let dir = write_session(&root, "foreign-session", "/other/proj", "no marker");
        let (scan, done) = scan(&root, "/tmp/proj", None);
        assert_eq!(scan, CaptureScan::None);
        assert!(done.contains(&dir));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_log_stays_transient() {
        let root = fixture_root();
        let now = Local::now();
        let dir = root
            .join(format!("{:04}", now.year()))
            .join(format!("{:02}", now.month()))
            .join(format!("{:02}", now.day()))
            .join("pending-session");
        std::fs::create_dir_all(&dir).unwrap();
        let (scan, done) = scan(&root, "/tmp/proj", None);
        assert_eq!(scan, CaptureScan::None);
        assert!(!done.contains(&dir));
        let _ = std::fs::remove_dir_all(&root);
    }
}
