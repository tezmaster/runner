use super::helpers::*;
use super::*;
#[cfg(test)]
use crate::golden::config_home as capture_home;
#[cfg(not(test))]
use runner_core::app_paths::home_dir as capture_home;

use runner_core::protocol::runtime_metadata::muse::{PERMISSIONS, SKILL_DIRS};

pub mod capture;
pub(crate) mod history;
pub(crate) mod usage;

pub struct Muse;
impl RuntimeAdapter for Muse {
    fn capture_prompt(
        &self,
        session_id: &str,
        first_turn: Option<String>,
    ) -> (Option<String>, Option<String>) {
        capture::mark_prompt(session_id, first_turn)
    }
    fn start_rollout_capture(&self, request: RolloutCaptureRequest) {
        capture::spawn_capture(request);
    }
    fn usage(&self) -> Option<&'static UsageSource> {
        Some(&USAGE)
    }
    fn token_history(&self) -> Option<&'static TokenHistory> {
        Some(&HISTORY)
    }

    fn skills(&self) -> SkillSupport {
        SkillSupport {
            roots: SKILL_DIRS,
            ..Default::default()
        }
    }

    fn key_capture(&self) -> KeyCapture {
        KeyCapture::RolloutScan {
            // Muse honors XDG_DATA_HOME; v1 pins the default location
            // (follow-up: read XDG_DATA_HOME from the spawn env).
            sessions_root: capture_home().map(|home| {
                home.join(".local")
                    .join("share")
                    .join("muse")
                    .join("sessions")
            }),
        }
    }

    fn catalog(&self) -> Option<RuntimeCatalog> {
        runner_core::protocol::runtime_metadata::muse::catalog()
    }
    fn permissions(&self) -> &'static Permissions {
        &PERMISSIONS
    }
    fn model_effort_args(&self, model: Option<&str>, effort: Option<&str>) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(m) = trim_some(model) {
            out.push("--model".to_string());
            out.push(m.to_string());
        }
        if let Some(e) = trim_some(effort) {
            // Muse's flag takes lowercase values (none|minimal|low|
            // medium|high|xhigh|max|ultra); the catalog already stores
            // them lowercase, but slot overrides may not.
            out.push("--reasoning-effort".to_string());
            out.push(e.to_ascii_lowercase());
        }
        out
    }
    fn first_turn_argv(&self, body: Option<&str>) -> Vec<String> {
        positional_first_turn(body)
    }
    fn resume_plan(&self, prior_key: Option<&str>) -> ResumePlan {
        subcommand_resume(prior_key)
    }
    fn launch_args(&self, ctx: &LaunchContext<'_>) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(self.model_effort_args(ctx.model, ctx.effort));
        out.extend(self.first_turn_argv(if ctx.resuming { None } else { ctx.first_turn }));
        out
    }
}

#[cfg(test)]
mod tests;

static USAGE: UsageSource = UsageSource {
    fetch: |command, env, _denied| usage::fetch_muse(command, env),
};

static HISTORY: TokenHistory = TokenHistory {
    read: history::read_since,
};
