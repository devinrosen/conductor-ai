use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::error::{ConductorError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkTarget {
    pub name: String,
    pub command: String,
    #[serde(rename = "type")]
    pub target_type: String,
}

/// Controls whether an agent is auto-started after creating a worktree from a ticket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoStartAgent {
    /// Prompt the user to confirm (default)
    #[default]
    Ask,
    /// Always start the agent automatically
    Always,
    /// Never auto-start
    Never,
}

fn default_work_targets() -> Vec<WorkTarget> {
    vec![WorkTarget {
        name: "VS Code".to_string(),
        command: "code".to_string(),
        target_type: "editor".to_string(),
    }]
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    #[serde(default)]
    pub post_run: PostRunConfig,
    #[serde(default)]
    pub github: GitHubSettings,
}

/// Top-level `[github]` section, currently containing only the optional App config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitHubSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<GitHubAppConfig>,
}

/// Configuration for posting comments as a GitHub App bot identity.
///
/// ```toml
/// [github.app]
/// app_id = 123456
/// client_id = "Iv23liXXXXXXXXXXXXXX"  # from App settings — required for newer apps
/// private_key_path = "~/.conductor/conductor-ai.pem"
/// installation_id = 789012
/// ```
///
/// The `client_id` (found on the GitHub App settings page) is used as the JWT
/// `iss` claim. Newer GitHub Apps require it; older apps can omit it and
/// `app_id` will be used as a fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubAppConfig {
    pub app_id: u64,
    /// Client ID from the GitHub App settings page (e.g. "Iv23li...").
    /// Used as the JWT `iss` claim. Required for newer GitHub Apps;
    /// falls back to `app_id` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub private_key_path: String,
    pub installation_id: u64,
}

/// Configuration for the automated post-agent lifecycle
/// (commit, PR, review loop, merge).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostRunConfig {
    /// Maximum review → fix iterations before giving up (default: 3).
    #[serde(default = "default_review_loop_max")]
    pub review_loop_max: u32,
    /// Model to use for commit message / PR description generation.
    #[serde(default = "default_commit_model")]
    pub commit_model: String,
    /// Commit message style: "conventional" or "free-form".
    #[serde(default = "default_commit_style")]
    pub commit_style: String,
    /// Issue labels that qualify for auto-merge (default: ["bug", "chore", "fix"]).
    #[serde(default = "default_auto_merge_labels")]
    pub auto_merge_labels: Vec<String>,
    /// Issue labels that require manual approval (default: ["enhancement", "feature"]).
    #[serde(default = "default_manual_merge_labels")]
    pub manual_merge_labels: Vec<String>,
    /// Allow `--dangerously-skip-permissions` when spawning fix agents (default: false).
    /// Only enable this if you trust all PR reviewers, as review comments drive the fix agent.
    #[serde(default)]
    pub dangerous_skip_permissions: bool,
}

impl Default for PostRunConfig {
    fn default() -> Self {
        Self {
            review_loop_max: default_review_loop_max(),
            commit_model: default_commit_model(),
            commit_style: default_commit_style(),
            auto_merge_labels: default_auto_merge_labels(),
            manual_merge_labels: default_manual_merge_labels(),
            dangerous_skip_permissions: false,
        }
    }
}

fn default_review_loop_max() -> u32 {
    3
}

fn default_commit_model() -> String {
    "claude-haiku-4-5-20251001".to_string()
}

fn default_commit_style() -> String {
    "conventional".to_string()
}

fn default_auto_merge_labels() -> Vec<String> {
    vec!["bug".to_string(), "chore".to_string(), "fix".to_string()]
}

fn default_manual_merge_labels() -> Vec<String> {
    vec!["enhancement".to_string(), "feature".to_string()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_workspace_root")]
    pub workspace_root: PathBuf,
    #[serde(default = "default_sync_interval")]
    pub sync_interval_minutes: u32,
    /// Deprecated: use `work_targets` instead. Kept for backward compatibility.
    #[serde(default, skip_serializing)]
    pub editor: Option<String>,
    #[serde(default = "default_work_targets")]
    pub work_targets: Vec<WorkTarget>,
    #[serde(default)]
    pub auto_start_agent: AutoStartAgent,
    /// Global default model for Claude agent runs (e.g. "sonnet", "claude-opus-4-6").
    /// Overridden by per-worktree and per-run model settings. Omit to use claude's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Whether to auto-inject session context (worktree, ticket, prior runs, recent commits)
    /// into agent prompts. Defaults to true; set to false to disable.
    #[serde(default = "default_true")]
    pub inject_startup_context: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultsConfig {
    #[serde(default = "default_branch")]
    pub default_branch: String,
    #[serde(default = "default_feat_prefix")]
    pub worktree_prefix_feat: String,
    #[serde(default = "default_fix_prefix")]
    pub worktree_prefix_fix: String,
}

fn default_workspace_root() -> PathBuf {
    conductor_dir().join("workspaces")
}

fn default_sync_interval() -> u32 {
    15
}

fn default_branch() -> String {
    "main".to_string()
}

fn default_feat_prefix() -> String {
    "feat-".to_string()
}

fn default_fix_prefix() -> String {
    "fix-".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            workspace_root: default_workspace_root(),
            sync_interval_minutes: default_sync_interval(),
            editor: None,
            work_targets: default_work_targets(),
            auto_start_agent: AutoStartAgent::default(),
            model: None,
            inject_startup_context: true,
        }
    }
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            default_branch: default_branch(),
            worktree_prefix_feat: default_feat_prefix(),
            worktree_prefix_fix: default_fix_prefix(),
        }
    }
}

/// Returns the Conductor data directory: ~/.conductor
pub fn conductor_dir() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".conductor")
}

/// Returns the path to the SQLite database.
pub fn db_path() -> PathBuf {
    conductor_dir().join("conductor.db")
}

/// Returns the path to the config file.
pub fn config_path() -> PathBuf {
    conductor_dir().join("config.toml")
}

/// Returns the directory for agent log files.
pub fn agent_log_dir() -> PathBuf {
    conductor_dir().join("agent-logs")
}

/// Returns the log file path for a given agent run ID.
///
/// Convention: `~/.conductor/agent-logs/{run_id}.log`
pub fn agent_log_path(run_id: &str) -> PathBuf {
    agent_log_dir().join(format!("{run_id}.log"))
}

/// Load config from disk, returning defaults if the file doesn't exist.
/// Handles backward compatibility: if the old `editor` field is present
/// and `work_targets` was not explicitly set, migrates the editor value
/// into a single work target.
pub fn load_config() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = std::fs::read_to_string(&path)?;
    let mut config: Config =
        toml::from_str(&contents).map_err(|e| ConductorError::Config(e.to_string()))?;

    // Backward compat: migrate old `editor` field to `work_targets`
    if let Some(ref editor) = config.general.editor {
        // Check if the raw TOML has work_targets explicitly set
        let raw: toml::Value =
            toml::from_str(&contents).map_err(|e| ConductorError::Config(e.to_string()))?;
        let has_work_targets = raw
            .get("general")
            .and_then(|g| g.get("work_targets"))
            .is_some();

        if !has_work_targets {
            config.general.work_targets = vec![WorkTarget {
                name: editor.clone(),
                command: editor.clone(),
                target_type: "editor".to_string(),
            }];
        }
    }
    config.general.editor = None;

    Ok(config)
}

/// Save config to disk.
pub fn save_config(config: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string_pretty(config)
        .map_err(|e| ConductorError::Config(format!("serialize config: {e}")))?;
    std::fs::write(&path, contents)?;
    Ok(())
}

/// Ensure the conductor data directory exists.
pub fn ensure_dirs(config: &Config) -> Result<()> {
    std::fs::create_dir_all(conductor_dir())?;
    std::fs::create_dir_all(&config.general.workspace_root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_start_agent_default() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.general.auto_start_agent, AutoStartAgent::Ask);
    }

    #[test]
    fn test_auto_start_agent_always() {
        let config: Config = toml::from_str(
            r#"
            [general]
            auto_start_agent = "always"
        "#,
        )
        .unwrap();
        assert_eq!(config.general.auto_start_agent, AutoStartAgent::Always);
    }

    #[test]
    fn test_auto_start_agent_never() {
        let config: Config = toml::from_str(
            r#"
            [general]
            auto_start_agent = "never"
        "#,
        )
        .unwrap();
        assert_eq!(config.general.auto_start_agent, AutoStartAgent::Never);
    }

    #[test]
    fn test_model_default_is_none() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.general.model, None);
    }

    #[test]
    fn test_model_can_be_set() {
        let config: Config = toml::from_str(
            r#"
            [general]
            model = "claude-sonnet-4-6"
        "#,
        )
        .unwrap();
        assert_eq!(config.general.model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn test_model_alias() {
        let config: Config = toml::from_str(
            r#"
            [general]
            model = "sonnet"
        "#,
        )
        .unwrap();
        assert_eq!(config.general.model.as_deref(), Some("sonnet"));
    }

    #[test]
    fn test_inject_startup_context_default_true() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.general.inject_startup_context);
    }

    #[test]
    fn test_inject_startup_context_opt_out() {
        let config: Config = toml::from_str(
            r#"
            [general]
            inject_startup_context = false
        "#,
        )
        .unwrap();
        assert!(!config.general.inject_startup_context);
    }

    #[test]
    fn test_post_run_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.post_run.review_loop_max, 3);
        assert_eq!(config.post_run.commit_model, "claude-haiku-4-5-20251001");
        assert_eq!(config.post_run.commit_style, "conventional");
        assert_eq!(
            config.post_run.auto_merge_labels,
            vec!["bug", "chore", "fix"]
        );
        assert_eq!(
            config.post_run.manual_merge_labels,
            vec!["enhancement", "feature"]
        );
    }

    #[test]
    fn test_post_run_custom() {
        let config: Config = toml::from_str(
            r#"
            [post_run]
            review_loop_max = 5
            commit_model = "claude-sonnet-4-6"
            commit_style = "free-form"
            auto_merge_labels = ["bug"]
            manual_merge_labels = ["feature", "epic"]
        "#,
        )
        .unwrap();
        assert_eq!(config.post_run.review_loop_max, 5);
        assert_eq!(config.post_run.commit_model, "claude-sonnet-4-6");
        assert_eq!(config.post_run.commit_style, "free-form");
        assert_eq!(config.post_run.auto_merge_labels, vec!["bug"]);
        assert_eq!(config.post_run.manual_merge_labels, vec!["feature", "epic"]);
    }

    #[test]
    fn test_github_app_default_none() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.github.app.is_none());
    }

    #[test]
    fn test_github_app_configured() {
        let config: Config = toml::from_str(
            r#"
            [github.app]
            app_id = 123456
            private_key_path = "~/.conductor/conductor-ai.pem"
            installation_id = 789012
        "#,
        )
        .unwrap();
        let app = config.github.app.unwrap();
        assert_eq!(app.app_id, 123456);
        assert!(app.client_id.is_none());
        assert_eq!(app.private_key_path, "~/.conductor/conductor-ai.pem");
        assert_eq!(app.installation_id, 789012);
    }

    #[test]
    fn test_github_app_with_client_id() {
        let config: Config = toml::from_str(
            r#"
            [github.app]
            app_id = 123456
            client_id = "Iv23liABCDEF12345678"
            private_key_path = "~/.conductor/conductor-ai.pem"
            installation_id = 789012
        "#,
        )
        .unwrap();
        let app = config.github.app.unwrap();
        assert_eq!(app.app_id, 123456);
        assert_eq!(app.client_id.as_deref(), Some("Iv23liABCDEF12345678"));
    }

    #[test]
    fn test_github_section_without_app() {
        let config: Config = toml::from_str(
            r#"
            [github]
        "#,
        )
        .unwrap();
        assert!(config.github.app.is_none());
    }
}
