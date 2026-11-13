// Muse's subscription allowance, read with `muse usage --json` (a local
// command: no model turn). One short window and one weekly window, each a
// whole-number percent used and a reset time. Muse's own `reason` field says
// why no allowance came back; only the wording is read, never acted on.

use crate::runtime_status::direct_chat_path;
use crate::shell_path::LoginShellEnv;
use crate::usage::{parse_timestamp, percent, UnavailableReason, UsageWindow, FETCH_TIMEOUT};
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn parse_window(value: Option<&Value>, name: &str) -> Option<UsageWindow> {
    let value = value?;
    Some(UsageWindow {
        name: name.to_owned(),
        used_percent: percent(value.get("usedPercent"))?,
        resets_at: parse_timestamp(value.get("resetsAtMs")),
    })
}

pub(crate) fn parse_muse(value: &Value) -> Result<Vec<UsageWindow>, UnavailableReason> {
    let allowance = value.get("allowance").filter(|allowance| !allowance.is_null());
    let Some(allowance) = allowance else {
        let reason = value
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        return Err(
            if ["sign", "log in", "login", "auth"]
                .iter()
                .any(|word| reason.contains(word))
            {
                UnavailableReason::SignIn
            } else {
                UnavailableReason::InvalidResponse
            },
        );
    };
    // The short window's length is not stated in the output, so it keeps a neutral name.
    let windows: Vec<_> = [("window", "Window"), ("weekly", "Week")]
        .into_iter()
        .filter_map(|(key, name)| parse_window(allowance.get(key), name))
        .collect();
    if windows.is_empty() {
        Err(UnavailableReason::InvalidResponse)
    } else {
        Ok(windows)
    }
}

pub(crate) fn fetch_muse(
    command: &str,
    env: &LoginShellEnv,
) -> Result<Vec<UsageWindow>, UnavailableReason> {
    fetch_muse_with_timeout(command, env, FETCH_TIMEOUT)
}

pub(crate) fn fetch_muse_with_timeout(
    command: &str,
    env: &LoginShellEnv,
    timeout: Duration,
) -> Result<Vec<UsageWindow>, UnavailableReason> {
    let mut process = Command::new(command);
    process
        .args(["usage", "--json"])
        .envs(&env.vars)
        .env("PATH", direct_chat_path(env))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(home) = runner_core::app_paths::home_dir() {
        process.current_dir(home);
    }
    let mut child = process
        .spawn()
        .map_err(|_| UnavailableReason::InvalidResponse)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or(UnavailableReason::InvalidResponse)?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        // A bounded read: the output is one short JSON object.
        let _ = stdout.by_ref().take(64 * 1024).read_to_string(&mut text);
        let _ = tx.send(text);
    });
    let text = match rx.recv_timeout(timeout) {
        Ok(text) => text,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(UnavailableReason::InvalidResponse);
        }
    };
    let _ = child.wait();
    let value: Value =
        serde_json::from_str(text.trim()).map_err(|_| UnavailableReason::InvalidResponse)?;
    parse_muse(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "schemaVersion": 1,
            "allowance": {
                "tier": "Muse Code High Usage",
                "window": {"usedPercent": 4, "resetsAtMs": 1794506456000_i64},
                "weekly": {"usedPercent": 43, "resetsAtMs": 1794787200000_i64},
                "asOfMs": 1794493510212_i64,
                "source": "mint"
            },
            "reason": null,
            "hint": null
        })
    }

    #[test]
    fn reads_both_windows_with_percent_and_reset() {
        let windows = parse_muse(&sample()).unwrap();
        assert_eq!(
            windows.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            ["Window", "Week"]
        );
        assert_eq!(windows[0].used_percent, 4.0);
        assert_eq!(windows[1].used_percent, 43.0);
        assert_eq!(
            windows[1].resets_at.unwrap().timestamp_millis(),
            1794787200000
        );
    }

    #[test]
    fn one_missing_window_still_reports_the_other() {
        let mut value = sample();
        value["allowance"].as_object_mut().unwrap().remove("window");
        let windows = parse_muse(&value).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].name, "Week");
    }

    #[test]
    fn percent_is_clamped_and_a_bad_window_is_skipped() {
        let mut value = sample();
        value["allowance"]["weekly"]["usedPercent"] = json!(130);
        value["allowance"]["window"]["usedPercent"] = json!("many");
        let windows = parse_muse(&value).unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, 100.0);
    }

    #[test]
    fn no_allowance_is_a_sign_in_prompt_or_an_invalid_response() {
        assert_eq!(
            parse_muse(&json!({"allowance": null, "reason": "Not signed in"})),
            Err(UnavailableReason::SignIn)
        );
        assert_eq!(
            parse_muse(&json!({"allowance": null, "reason": "service down"})),
            Err(UnavailableReason::InvalidResponse)
        );
        assert_eq!(parse_muse(&json!({})), Err(UnavailableReason::InvalidResponse));
        assert_eq!(
            parse_muse(&json!({"allowance": {"tier": "x"}})),
            Err(UnavailableReason::InvalidResponse)
        );
    }

    #[cfg(unix)]
    fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn the_command_is_run_with_usage_json_and_its_output_read() {
        let dir = tempfile::tempdir().unwrap();
        let env = LoginShellEnv::default();
        let ok = script(
            dir.path(),
            "muse-ok",
            r#"[ "$1 $2" = "usage --json" ] || exit 3
echo '{"allowance":{"window":{"usedPercent":9,"resetsAtMs":1794506456000},"weekly":{"usedPercent":12,"resetsAtMs":1794787200000}}}'"#,
        );
        let windows = fetch_muse_with_timeout(&ok, &env, Duration::from_secs(5)).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[1].used_percent, 12.0);
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_garbled_or_missing_command_gives_no_windows() {
        let dir = tempfile::tempdir().unwrap();
        let env = LoginShellEnv::default();
        let hung = script(dir.path(), "muse-hung", "sleep 30");
        let started = std::time::Instant::now();
        assert_eq!(
            fetch_muse_with_timeout(&hung, &env, Duration::from_millis(300)),
            Err(UnavailableReason::InvalidResponse)
        );
        assert!(started.elapsed() < Duration::from_secs(5), "the hung command was not cut off");
        let garbled = script(dir.path(), "muse-garbled", "echo not json");
        assert_eq!(
            fetch_muse_with_timeout(&garbled, &env, Duration::from_secs(5)),
            Err(UnavailableReason::InvalidResponse)
        );
        assert_eq!(
            fetch_muse_with_timeout("/no/such/muse", &env, Duration::from_secs(1)),
            Err(UnavailableReason::InvalidResponse)
        );
    }
}
