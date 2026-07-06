use anyhow::Error;
use executors::{executors::BaseCodingAgent, profile::ExecutorProfileId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
pub use v7::{
    EditorConfig, EditorType, GitHubConfig, NotificationConfig, ShowcaseState, SoundFile,
    ThemeMode, UiLanguage,
};

use crate::services::config::versions::v7;

fn default_git_branch_prefix() -> String {
    "vk".to_string()
}

fn default_pr_auto_description_enabled() -> bool {
    true
}

fn default_commit_reminder_enabled() -> bool {
    true
}

fn default_relay_enabled() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, TS, PartialEq, Eq)]
pub enum SendMessageShortcut {
    #[default]
    ModifierEnter,
    Enter,
}

/// Linear integration credentials, modeled as a KEYED collection (multi-account
/// from day one) — deliberately NOT a single-account blob like `GitHubConfig`.
/// Keyed by a short account key (e.g. "personal", "work"). See ADR 0002 (JM-718).
#[derive(Clone, Debug, Default, Serialize, Deserialize, TS)]
pub struct LinearConfig {
    #[serde(default)]
    pub accounts: std::collections::HashMap<String, LinearAccount>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, TS)]
pub struct LinearAccount {
    /// Linear API token (personal API key or OAuth access token). Plaintext at
    /// rest (local-only, per operator decision); MUST be redacted before this
    /// Config is returned over the wire — see `Config::redacted`.
    #[serde(default)]
    pub token: Option<String>,
    /// Display label for this account's Linear workspace.
    #[serde(default)]
    pub workspace_name: Option<String>,
    /// Linear team id whose workflow states `state_map` targets (v1: one team
    /// per account).
    #[serde(default)]
    pub team_id: Option<String>,
    /// Board status -> Linear workflow-state id. Keyed by `project_statuses.id`
    /// (a stable UUID string), NOT status name — names are per-project and
    /// user-editable.
    #[serde(default)]
    pub state_map: std::collections::HashMap<String, String>,
    /// Inbound import (JM-734): the project that assigned/labelled Linear issues
    /// import into. `None` disables inbound for this account (never guess a
    /// project — account→project is N:1). MUST be a project bound to this account
    /// (`projects.linear_account_key == <this key>`); enforced at the config
    /// route and re-checked at sweep time to survive a later rebind.
    #[serde(default)]
    pub import_target_project_id: Option<String>,
    /// Inbound import filter (JM-734): also import issues carrying this label
    /// (in addition to assigned-to-me). `None` = assigned-to-me only.
    #[serde(default)]
    pub import_label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct Config {
    pub config_version: String,
    pub theme: ThemeMode,
    pub executor_profile: ExecutorProfileId,
    pub disclaimer_acknowledged: bool,
    pub onboarding_acknowledged: bool,
    #[serde(default)]
    pub remote_onboarding_acknowledged: bool,
    pub notifications: NotificationConfig,
    pub editor: EditorConfig,
    pub github: GitHubConfig,
    pub analytics_enabled: bool,
    pub workspace_dir: Option<String>,
    pub last_app_version: Option<String>,
    pub show_release_notes: bool,
    #[serde(default)]
    pub language: UiLanguage,
    #[serde(default = "default_git_branch_prefix")]
    pub git_branch_prefix: String,
    #[serde(default)]
    pub showcases: ShowcaseState,
    #[serde(default = "default_pr_auto_description_enabled")]
    pub pr_auto_description_enabled: bool,
    #[serde(default)]
    pub pr_auto_description_prompt: Option<String>,
    #[serde(default = "default_commit_reminder_enabled")]
    pub commit_reminder_enabled: bool,
    #[serde(default)]
    pub commit_reminder_prompt: Option<String>,
    #[serde(default)]
    pub send_message_shortcut: SendMessageShortcut,
    #[serde(default = "default_relay_enabled")]
    pub relay_enabled: bool,
    #[serde(default)]
    pub host_nickname: Option<String>,
    #[serde(default)]
    pub linear: LinearConfig,
}

impl Config {
    /// A clone of this config safe to return over the wire: Linear account
    /// tokens are stripped. Any handler returning `Config` to a client (e.g.
    /// `GET /api/info`) MUST use this. Linear accounts are managed only via
    /// their dedicated routes, never round-tripped through `PUT /config`.
    pub fn redacted(&self) -> Self {
        let mut cfg = self.clone();
        for account in cfg.linear.accounts.values_mut() {
            account.token = None;
        }
        cfg
    }

    fn from_v7_config(old_config: v7::Config) -> Self {
        // Convert Option<bool> to bool: None or Some(true) become true, Some(false) stays false
        let analytics_enabled = old_config.analytics_enabled.unwrap_or(true);

        Self {
            config_version: "v8".to_string(),
            theme: old_config.theme,
            executor_profile: old_config.executor_profile,
            disclaimer_acknowledged: old_config.disclaimer_acknowledged,
            onboarding_acknowledged: old_config.onboarding_acknowledged,
            remote_onboarding_acknowledged: false,
            notifications: old_config.notifications,
            editor: old_config.editor,
            github: old_config.github,
            analytics_enabled,
            workspace_dir: old_config.workspace_dir,
            last_app_version: old_config.last_app_version,
            show_release_notes: old_config.show_release_notes,
            language: old_config.language,
            git_branch_prefix: old_config.git_branch_prefix,
            showcases: old_config.showcases,
            pr_auto_description_enabled: true,
            pr_auto_description_prompt: None,
            commit_reminder_enabled: true,
            commit_reminder_prompt: None,
            send_message_shortcut: SendMessageShortcut::default(),
            relay_enabled: true,
            host_nickname: None,
            linear: LinearConfig::default(),
        }
    }

    pub fn from_previous_version(raw_config: &str) -> Result<Self, Error> {
        let old_config = v7::Config::from(raw_config.to_string());
        Ok(Self::from_v7_config(old_config))
    }
}

impl From<String> for Config {
    fn from(raw_config: String) -> Self {
        if let Ok(config) = serde_json::from_str::<Config>(&raw_config)
            && config.config_version == "v8"
        {
            return config;
        }

        match Self::from_previous_version(&raw_config) {
            Ok(config) => {
                tracing::info!("Config upgraded to v8");
                config
            }
            Err(e) => {
                tracing::warn!("Config migration failed: {}, using default", e);
                Self::default()
            }
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: "v8".to_string(),
            theme: ThemeMode::System,
            executor_profile: ExecutorProfileId::new(BaseCodingAgent::ClaudeCode),
            disclaimer_acknowledged: false,
            onboarding_acknowledged: false,
            remote_onboarding_acknowledged: false,
            notifications: NotificationConfig::default(),
            editor: EditorConfig::default(),
            github: GitHubConfig::default(),
            analytics_enabled: true,
            workspace_dir: None,
            last_app_version: None,
            show_release_notes: false,
            language: UiLanguage::default(),
            git_branch_prefix: default_git_branch_prefix(),
            showcases: ShowcaseState::default(),
            pr_auto_description_enabled: true,
            pr_auto_description_prompt: None,
            commit_reminder_enabled: true,
            commit_reminder_prompt: None,
            send_message_shortcut: SendMessageShortcut::default(),
            relay_enabled: true,
            host_nickname: None,
            linear: LinearConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_strips_linear_tokens_but_preserves_the_rest() {
        let mut cfg = Config::default();
        cfg.linear.accounts.insert(
            "work".to_string(),
            LinearAccount {
                token: Some("lin_api_secret".to_string()),
                workspace_name: Some("OneOnMe".to_string()),
                team_id: Some("team-123".to_string()),
                state_map: std::collections::HashMap::from([(
                    "status-uuid".to_string(),
                    "linear-state-uuid".to_string(),
                )]),
                ..Default::default()
            },
        );

        let redacted = cfg.redacted();
        let acct = redacted.linear.accounts.get("work").unwrap();

        // Token stripped on the wire...
        assert_eq!(acct.token, None);
        // ...but non-secret fields preserved so the UI can still render the account.
        assert_eq!(acct.workspace_name.as_deref(), Some("OneOnMe"));
        assert_eq!(acct.team_id.as_deref(), Some("team-123"));
        assert_eq!(
            acct.state_map.get("status-uuid").map(String::as_str),
            Some("linear-state-uuid")
        );

        // The original (persisted/in-memory) config still holds the real token.
        assert_eq!(
            cfg.linear.accounts.get("work").unwrap().token.as_deref(),
            Some("lin_api_secret")
        );
    }

    #[test]
    fn inbound_import_fields_default_on_pre_jm734_account() {
        // A LinearAccount persisted before JM-734 has no import_* keys. They must
        // deserialize as None via #[serde(default)] rather than failing the parse.
        let acct: LinearAccount =
            serde_json::from_str(r#"{"token":"lin_api_x","team_id":"t1","state_map":{}}"#).unwrap();
        assert_eq!(acct.import_target_project_id, None);
        assert_eq!(acct.import_label, None);
        assert_eq!(acct.team_id.as_deref(), Some("t1"));
    }
}
