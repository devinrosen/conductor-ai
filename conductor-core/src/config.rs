use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::{ConductorError, Result};

/// Controls which permission flag is passed to Claude Code when launching agent runs.
///
/// ```toml
/// [general]
/// agent_permission_mode = "skip-permissions" # default — uses --dangerously-skip-permissions
/// agent_permission_mode = "auto-mode"        # uses --enable-auto-mode (may prompt in headless agents)
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentPermissionMode {
    /// Use `--enable-auto-mode` (may prompt for permissions in headless agents).
    AutoMode,
    /// Use `--dangerously-skip-permissions` (default for headless agent runs).
    #[default]
    SkipPermissions,
    /// Use `--permission-mode plan` (read-only mode for repo-scoped agents).
    Plan,
    /// Use `--dangerously-skip-permissions` + `--allowedTools` read-safe pattern.
    /// Excludes file-writing tools (Edit, Write, MultiEdit, NotebookEdit) at the
    /// Claude tool level without locking into plan-mode's "propose before acting"
    /// flow, so Bash/gh remain fully executable.
    RepoSafe,
}

impl AgentPermissionMode {
    /// Returns the conductor CLI flag for this mode (used in `conductor agent run` passthrough args).
    pub fn cli_flag(&self) -> &str {
        match self {
            Self::AutoMode => "--enable-auto-mode",
            Self::SkipPermissions => "--dangerously-skip-permissions",
            Self::Plan => "--permission-mode",
            Self::RepoSafe => "--permission-mode",
        }
    }

    /// Returns the optional value argument that follows the conductor CLI flag.
    pub fn cli_flag_value(&self) -> Option<&str> {
        match self {
            Self::Plan => Some("plan"),
            Self::RepoSafe => Some("repo-safe"),
            _ => None,
        }
    }

    /// Returns the actual permission flag to pass to the `claude` subprocess.
    ///
    /// This differs from `cli_flag()` for `RepoSafe`: conductor receives
    /// `--permission-mode repo-safe`, but claude receives `--dangerously-skip-permissions`.
    pub fn claude_permission_flag(&self) -> &str {
        match self {
            Self::AutoMode => "--enable-auto-mode",
            Self::SkipPermissions => "--dangerously-skip-permissions",
            Self::Plan => "--permission-mode",
            Self::RepoSafe => "--dangerously-skip-permissions",
        }
    }

    /// Returns the optional value argument that follows the claude permission flag.
    pub fn claude_permission_flag_value(&self) -> Option<&str> {
        match self {
            Self::Plan => Some("plan"),
            _ => None,
        }
    }

    /// Returns the `--allowedTools` pattern for this mode, if any.
    ///
    /// Plan and RepoSafe modes allow read-only and shell tools (Bash, Glob, Grep, Read,
    /// WebFetch, WebSearch) plus all MCP tools, while excluding file-writing tools
    /// (Edit, Write, MultiEdit, NotebookEdit).
    pub fn allowed_tools(&self) -> Option<&'static str> {
        match self {
            Self::Plan | Self::RepoSafe => {
                Some("Bash,Glob,Grep,Read,WebFetch,WebSearch,mcp__conductor__*,mcp__*")
            }
            _ => None,
        }
    }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotificationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub workflows: WorkflowNotificationConfig,
    #[serde(default)]
    pub slack: SlackConfig,
    /// Base URL of the conductor web UI used to build deep links in notification
    /// events (e.g. `https://conductor.myhost.ts.net`). Trailing slash is trimmed
    /// automatically. When set, workflow completion/failure notifications include a
    /// `url` field of the form `{web_url}/repos/{repo_id}/worktrees/{worktree_id}/workflows/runs/{run_id}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SlackConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
    /// Slack app signing secret for verifying slash command request signatures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_secret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNotificationConfig {
    #[serde(default)]
    pub on_success: bool,
    #[serde(default = "default_true")]
    pub on_failure: bool,
    #[serde(default = "default_true")]
    pub on_gate_human: bool,
    #[serde(default)]
    pub on_gate_ci: bool,
    #[serde(default = "default_true")]
    pub on_gate_pr_review: bool,
}

impl Default for WorkflowNotificationConfig {
    fn default() -> Self {
        Self {
            on_success: false,
            on_failure: true,
            on_gate_human: true,
            on_gate_ci: false,
            on_gate_pr_review: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebPushConfig {
    /// VAPID public key (base64url encoded)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vapid_public_key: Option<String>,
    /// VAPID private key (base64url encoded)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vapid_private_key: Option<String>,
    /// Subject for VAPID (typically a mailto: or https: URL)
    #[serde(default)]
    pub vapid_subject: Option<String>,
}

/// Configuration for a single notification hook (shell or HTTP).
///
/// ```toml
/// [[notify.hooks]]
/// on = "workflow_run.*"
/// run = "~/.conductor/hooks/notify.sh"
///
/// [[notify.hooks]]
/// on = "gate.waiting"
/// url = "https://hooks.example.com/conductor"
/// headers = { Authorization = "$CONDUCTOR_HOOK_TOKEN" }
/// timeout_ms = 5000
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookConfig {
    /// Glob pattern for event names, e.g. `"*"`, `"workflow_run.*"`, `"gate.waiting"`.
    pub on: String,
    /// Shell command to run (passed to `sh -c`). Receives `CONDUCTOR_*` env vars.
    #[serde(default)]
    pub run: Option<String>,
    /// URL to POST JSON payload to.
    #[serde(default)]
    pub url: Option<String>,
    /// HTTP headers; values starting with `$` are resolved from environment.
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    /// Request/process timeout in milliseconds. Defaults to 10 000.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// For `cost_spike` / `duration_spike`: minimum multiple over baseline to trigger.
    #[serde(default)]
    pub threshold_multiple: Option<f64>,
    /// For `gate.pending_too_long`: fire after gate has been waiting this many ms.
    #[serde(default)]
    pub gate_pending_ms: Option<u64>,
    /// Optional workflow name filter: only fire for events from this workflow.
    #[serde(default)]
    pub workflow: Option<String>,
    /// When `true`, only fire for root workflow runs (`parent_workflow_run_id` is `None`).
    /// `None` or `false` fires for all workflows (backwards compatible default).
    #[serde(default)]
    pub root_workflows_only: Option<bool>,
    /// Only fire for events from this repo (exact match on repo_slug).
    #[serde(default)]
    pub repo: Option<String>,
    /// Only fire for events from this branch (glob pattern, e.g. `"release/*"`).
    #[serde(default)]
    pub branch: Option<String>,
    /// For gate events: only fire for this step name (exact match).
    #[serde(default)]
    pub step: Option<String>,
}

/// Top-level `[notify]` section containing user-configured notification hooks.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotifyConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    #[serde(default)]
    pub github: GitHubSettings,
    #[serde(default)]
    pub notifications: NotificationConfig,
    #[serde(default)]
    pub web_push: WebPushConfig,
    #[serde(default)]
    pub notify: NotifyConfig,
}

/// Top-level `[github]` section.
///
/// Supports a single `[github.app]` identity (original) and a named map
/// `[github.apps.<name>]` for multiple bot identities. Both forms can coexist;
/// named apps take precedence when looked up by name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitHubSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<GitHubAppConfig>,
    /// Named GitHub App identities, e.g. `[github.apps.developer]`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub apps: HashMap<String, GitHubAppConfig>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_workspace_root")]
    pub workspace_root: PathBuf,
    #[serde(default = "default_sync_interval")]
    pub sync_interval_minutes: u32,
    #[serde(default)]
    pub auto_start_agent: AutoStartAgent,
    /// Which permission flag to pass to Claude Code for agent runs.
    /// Defaults to `auto-mode` (`--enable-auto-mode`).
    #[serde(default)]
    pub agent_permission_mode: AgentPermissionMode,
    /// Global default model for Claude agent runs (e.g. "sonnet", "claude-opus-4-6").
    /// Overridden by per-worktree and per-run model settings. Omit to use claude's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Whether to auto-inject session context (worktree, ticket, prior runs, recent commits)
    /// into agent prompts. Defaults to true; set to false to disable.
    #[serde(default = "default_true")]
    pub inject_startup_context: bool,
    /// TUI color theme. One of: "conductor" (default), "nord", "gruvbox", "catppuccin_mocha",
    /// or the stem of a file in `~/.conductor/themes/`. Omit to use the default conductor theme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Automatically detect merged PRs and clean up worktrees (delete local/remote branch,
    /// remove worktree directory, auto-close orphaned features). Defaults to true.
    #[serde(default = "default_true")]
    pub auto_cleanup_merged_branches: bool,
    /// Custom Claude Code configuration directory (e.g. `~/.claude-personal`).
    /// When set, conductor uses this directory for MCP server setup and passes
    /// `CLAUDE_CONFIG_DIR` to agent runs. Defaults to `~/.claude` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_config_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultsConfig {
    #[serde(default = "default_branch")]
    pub default_branch: String,
    #[serde(default = "default_feat_prefix")]
    pub worktree_prefix_feat: String,
    #[serde(default = "default_fix_prefix")]
    pub worktree_prefix_fix: String,
    /// Number of days after which an active feature with no recent activity is
    /// considered stale. Set to 0 to disable stale detection.
    #[serde(default = "default_stale_feature_days")]
    pub stale_feature_days: u32,
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

fn default_stale_feature_days() -> u32 {
    14
}

fn default_true() -> bool {
    true
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            workspace_root: default_workspace_root(),
            sync_interval_minutes: default_sync_interval(),
            auto_start_agent: AutoStartAgent::default(),
            agent_permission_mode: AgentPermissionMode::default(),
            model: None,
            inject_startup_context: true,
            theme: None,
            auto_cleanup_merged_branches: true,
            claude_config_dir: None,
        }
    }
}

impl GeneralConfig {
    /// Returns the resolved Claude config directory as a `PathBuf`.
    ///
    /// If `claude_config_dir` is set, expands `~` and returns the result.
    /// Otherwise falls back to `~/.claude`.
    pub fn resolved_claude_config_dir(&self) -> Result<PathBuf> {
        match self.custom_claude_config_dir() {
            Some(result) => result,
            None => dirs::home_dir()
                .map(|h| h.join(".claude"))
                .ok_or_else(|| ConductorError::Config("cannot determine home directory".into())),
        }
    }

    /// Returns the custom Claude config directory only when explicitly configured.
    ///
    /// Returns `None` when `claude_config_dir` is not set (use the default `~/.claude`).
    /// Returns `Some(Ok(path))` when configured and tilde-expansion succeeds.
    /// Returns `Some(Err(...))` when configured but tilde-expansion fails.
    ///
    /// Prefer this over accessing `claude_config_dir` directly — callers should
    /// never need to inspect the raw field to distinguish "not configured" from
    /// "resolution error".
    pub fn custom_claude_config_dir(&self) -> Option<Result<PathBuf>> {
        self.claude_config_dir
            .as_deref()
            .map(|raw| crate::text_util::expand_tilde(raw).map_err(ConductorError::Config))
    }

    /// Resolves the custom Claude config directory, logging a warning and returning `None` on error.
    ///
    /// Returns `None` when no custom directory is configured or when resolution fails.
    /// Callers that need to distinguish the error case should use [`custom_claude_config_dir`] instead.
    pub fn resolve_optional_claude_dir(&self) -> Option<PathBuf> {
        match self.custom_claude_config_dir() {
            Some(Ok(dir)) => Some(dir),
            Some(Err(e)) => {
                tracing::warn!(
                    "failed to resolve claude_config_dir — will use default ~/.claude: {e}"
                );
                None
            }
            None => None,
        }
    }
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            default_branch: default_branch(),
            worktree_prefix_feat: default_feat_prefix(),
            worktree_prefix_fix: default_fix_prefix(),
            stale_feature_days: default_stale_feature_days(),
        }
    }
}

/// Returns the Conductor data directory: ~/.conductor
///
/// The result is cached after the first call so that repeated invocations
/// (e.g. inside loops that call `agent_log_path`) avoid redundant OS-level
/// `home_dir()` lookups.
///
/// The `CONDUCTOR_HOME` environment variable overrides the default location.
/// This is used by CLI integration tests to point each subprocess at an
/// isolated temp directory without touching the developer's real data.
pub fn conductor_dir() -> &'static PathBuf {
    static CONDUCTOR_DIR: OnceLock<PathBuf> = OnceLock::new();
    CONDUCTOR_DIR.get_or_init(|| {
        if let Ok(home) = std::env::var("CONDUCTOR_HOME") {
            return PathBuf::from(home);
        }
        dirs::home_dir()
            .expect("could not determine home directory")
            .join(".conductor")
    })
}

/// Returns the path to the SQLite database.
///
/// When the `CONDUCTOR_DB_PATH` environment variable is set to a non-empty
/// value, uses that path directly. Otherwise returns the global
/// `~/.conductor/conductor.db`.
///
/// The default global path ensures that repos, tickets, and workflow runs
/// are accessible regardless of the current working directory (including
/// from within worktrees where workflow script steps execute).
///
/// Use `CONDUCTOR_DB_PATH` for isolated migration testing against a local
/// database with seed data, without affecting the production DB:
///
/// ```sh
/// CONDUCTOR_DB_PATH=/tmp/test.db conductor tickets list
/// ```
pub fn db_path() -> PathBuf {
    if let Ok(custom) = std::env::var("CONDUCTOR_DB_PATH") {
        if !custom.is_empty() {
            return PathBuf::from(custom);
        }
    }
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

/// Returns the directory for user-supplied theme files: ~/.conductor/themes/
pub fn themes_dir() -> PathBuf {
    conductor_dir().join("themes")
}

/// Returns the log file path for a given agent run ID.
///
/// Convention: `~/.conductor/agent-logs/{run_id}.log`
pub fn agent_log_path(run_id: &str) -> PathBuf {
    agent_log_dir().join(format!("{run_id}.log"))
}

/// Load config from disk, returning defaults if the file doesn't exist.
pub fn load_config() -> Result<Config> {
    load_config_from(&config_path())
}

fn load_config_from(path: &std::path::Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = std::fs::read_to_string(path)?;
    let config: Config =
        toml::from_str(&contents).map_err(|e| ConductorError::Config(e.to_string()))?;

    // Parse raw TOML once for migration checks and github.app validation.
    let raw: toml::Value =
        toml::from_str(&contents).map_err(|e| ConductorError::Config(e.to_string()))?;

    // Guard: if [github.app] is present in the raw TOML but deserialized to None,
    // serde silently swallowed a deserialization error. Re-attempt explicitly so
    // the user gets a loud, actionable error instead of silent data loss.
    let raw_has_github_app = raw.get("github").and_then(|g| g.get("app")).is_some();
    if raw_has_github_app && config.github.app.is_none() {
        let app_value = raw.get("github").unwrap().get("app").unwrap().clone();
        let _: GitHubAppConfig = app_value.try_into().map_err(|e: toml::de::Error| {
            ConductorError::Config(format!("[github.app] failed to deserialize: {e}"))
        })?;
    }

    // Guard: validate each entry in [github.apps.<name>] that deserialized to empty.
    // Mirrors the single-app guard above for the named map case.
    if let Some(raw_apps) = raw.get("github").and_then(|g| g.get("apps")) {
        if let Some(apps_table) = raw_apps.as_table() {
            for (name, raw_value) in apps_table {
                if !config.github.apps.contains_key(name.as_str()) {
                    let _: GitHubAppConfig =
                        raw_value.clone().try_into().map_err(|e: toml::de::Error| {
                            ConductorError::Config(format!(
                                "[github.apps.{name}] failed to deserialize: {e}"
                            ))
                        })?;
                }
            }
        }
    }

    Ok(config)
}

/// Save config to disk.
///
/// Performs a patch-write: reads the existing file as `toml::Value`, merges
/// the serialized new config on top (known sections overwrite, unknown sections
/// are preserved), and writes the result back. This prevents sections that are
/// `None` in the Rust struct (e.g. `[github.app]`) from being erased when the
/// config is saved.
pub fn save_config(config: &Config) -> Result<()> {
    save_config_to(config, &config_path())
}

/// Recursively merge `new` into `base`.
///
/// - When both values are Tables, new keys overwrite base keys and keys absent
///   from `new` are preserved in `base` (so unknown/None sub-sections survive).
/// - For all other value types the new value wins outright.
fn merge_toml(base: &mut toml::Value, new: toml::Value) {
    match (base, new) {
        (toml::Value::Table(base_tbl), toml::Value::Table(new_tbl)) => {
            for (key, new_val) in new_tbl {
                match base_tbl.get_mut(&key) {
                    Some(base_val) => merge_toml(base_val, new_val),
                    None => {
                        base_tbl.insert(key, new_val);
                    }
                }
            }
        }
        (base, new) => *base = new,
    }
}

fn save_config_to(config: &Config, path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Start with whatever is currently on disk (preserves unknown sections).
    let mut merged: toml::Value = if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        toml::from_str(&existing)
            .map_err(|e| ConductorError::Config(format!("existing config is malformed: {e}")))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    // Serialize the new config to a Value and merge recursively.
    let new_value: toml::Value = toml::Value::try_from(config)
        .map_err(|e| ConductorError::Config(format!("serialize config: {e}")))?;

    merge_toml(&mut merged, new_value);

    let contents = toml::to_string_pretty(&merged)
        .map_err(|e| ConductorError::Config(format!("serialize config: {e}")))?;
    std::fs::write(path, contents)?;
    Ok(())
}

/// Ensure the conductor data directory exists.
pub fn ensure_dirs(config: &Config) -> Result<()> {
    std::fs::create_dir_all(conductor_dir())?;
    std::fs::create_dir_all(&config.general.workspace_root)?;
    std::fs::create_dir_all(themes_dir())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-repo .conductor/config.toml
// ---------------------------------------------------------------------------

/// Per-repo configuration loaded from `<repo_root>/.conductor/config.toml`.
///
/// All fields are optional — absent keys fall through to global [`Config`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoConfig {
    #[serde(default)]
    pub defaults: RepoDefaults,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_merge_strategy: Option<String>,
}

impl RepoConfig {
    /// Load repo-level config from `<repo_root>/.conductor/config.toml`.
    /// Returns defaults (all-None) if the file doesn't exist.
    pub fn load(repo_path: &Path) -> Result<RepoConfig> {
        let path = repo_path.join(".conductor").join("config.toml");
        if !path.exists() {
            return Ok(RepoConfig::default());
        }
        let contents = std::fs::read_to_string(&path)?;
        let config: RepoConfig =
            toml::from_str(&contents).map_err(|e| ConductorError::Config(e.to_string()))?;
        Ok(config)
    }

    /// Save repo-level config to `<repo_root>/.conductor/config.toml`.
    /// Creates the `.conductor/` directory if needed.
    pub fn save(&self, repo_path: &Path) -> Result<()> {
        let dir = repo_path.join(".conductor");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("config.toml");

        // Patch-write: preserve unknown sections
        let mut merged: toml::Value = if path.exists() {
            let existing = std::fs::read_to_string(&path)?;
            toml::from_str(&existing).map_err(|e| {
                ConductorError::Config(format!("existing repo config is malformed: {e}"))
            })?
        } else {
            toml::Value::Table(toml::map::Map::new())
        };

        let new_value: toml::Value = toml::Value::try_from(self)
            .map_err(|e| ConductorError::Config(format!("serialize repo config: {e}")))?;
        merge_toml(&mut merged, new_value);

        // Explicitly remove keys that are None in the struct but survived merge
        // (because skip_serializing_if omits them, so merge_toml preserves stale keys).
        if let Some(defaults) = merged.get_mut("defaults").and_then(|d| d.as_table_mut()) {
            if self.defaults.model.is_none() {
                defaults.remove("model");
            }
            if self.defaults.default_branch.is_none() {
                defaults.remove("default_branch");
            }
            if self.defaults.bot_name.is_none() {
                defaults.remove("bot_name");
            }
            if self.defaults.feature_merge_strategy.is_none() {
                defaults.remove("feature_merge_strategy");
            }
        }

        let contents = toml::to_string_pretty(&merged)
            .map_err(|e| ConductorError::Config(format!("serialize repo config: {e}")))?;
        std::fs::write(&path, contents)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes tests that mutate CONDUCTOR_DB_PATH to prevent races.
    static DB_PATH_ENV_LOCK: Mutex<()> = Mutex::new(());

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
    fn test_agent_permission_mode_default() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(
            config.general.agent_permission_mode,
            AgentPermissionMode::SkipPermissions
        );
    }

    #[test]
    fn test_agent_permission_mode_auto_mode() {
        let config: Config = toml::from_str(
            r#"
            [general]
            agent_permission_mode = "auto-mode"
        "#,
        )
        .unwrap();
        assert_eq!(
            config.general.agent_permission_mode,
            AgentPermissionMode::AutoMode
        );
    }

    #[test]
    fn test_agent_permission_mode_skip_permissions() {
        let config: Config = toml::from_str(
            r#"
            [general]
            agent_permission_mode = "skip-permissions"
        "#,
        )
        .unwrap();
        assert_eq!(
            config.general.agent_permission_mode,
            AgentPermissionMode::SkipPermissions
        );
    }

    #[test]
    fn test_agent_permission_mode_cli_flag_auto() {
        assert_eq!(
            AgentPermissionMode::AutoMode.cli_flag(),
            "--enable-auto-mode"
        );
    }

    #[test]
    fn test_agent_permission_mode_cli_flag_skip() {
        assert_eq!(
            AgentPermissionMode::SkipPermissions.cli_flag(),
            "--dangerously-skip-permissions"
        );
    }

    #[test]
    fn test_agent_permission_mode_cli_flag_plan() {
        assert_eq!(AgentPermissionMode::Plan.cli_flag(), "--permission-mode");
    }

    #[test]
    fn test_agent_permission_mode_cli_flag_repo_safe() {
        assert_eq!(
            AgentPermissionMode::RepoSafe.cli_flag(),
            "--permission-mode"
        );
    }

    #[test]
    fn test_agent_permission_mode_cli_flag_value() {
        assert_eq!(AgentPermissionMode::AutoMode.cli_flag_value(), None);
        assert_eq!(AgentPermissionMode::SkipPermissions.cli_flag_value(), None);
        assert_eq!(AgentPermissionMode::Plan.cli_flag_value(), Some("plan"));
        assert_eq!(
            AgentPermissionMode::RepoSafe.cli_flag_value(),
            Some("repo-safe")
        );
    }

    #[test]
    fn test_agent_permission_mode_claude_permission_flag() {
        assert_eq!(
            AgentPermissionMode::AutoMode.claude_permission_flag(),
            "--enable-auto-mode"
        );
        assert_eq!(
            AgentPermissionMode::SkipPermissions.claude_permission_flag(),
            "--dangerously-skip-permissions"
        );
        assert_eq!(
            AgentPermissionMode::Plan.claude_permission_flag(),
            "--permission-mode"
        );
        assert_eq!(
            AgentPermissionMode::RepoSafe.claude_permission_flag(),
            "--dangerously-skip-permissions"
        );
    }

    #[test]
    fn test_agent_permission_mode_claude_permission_flag_value() {
        assert_eq!(
            AgentPermissionMode::AutoMode.claude_permission_flag_value(),
            None
        );
        assert_eq!(
            AgentPermissionMode::SkipPermissions.claude_permission_flag_value(),
            None
        );
        assert_eq!(
            AgentPermissionMode::Plan.claude_permission_flag_value(),
            Some("plan")
        );
        assert_eq!(
            AgentPermissionMode::RepoSafe.claude_permission_flag_value(),
            None
        );
    }

    #[test]
    fn test_agent_permission_mode_allowed_tools() {
        assert_eq!(AgentPermissionMode::AutoMode.allowed_tools(), None);
        assert_eq!(AgentPermissionMode::SkipPermissions.allowed_tools(), None);
        assert_eq!(
            AgentPermissionMode::Plan.allowed_tools(),
            Some("Bash,Glob,Grep,Read,WebFetch,WebSearch,mcp__conductor__*,mcp__*")
        );
        assert_eq!(
            AgentPermissionMode::RepoSafe.allowed_tools(),
            Some("Bash,Glob,Grep,Read,WebFetch,WebSearch,mcp__conductor__*,mcp__*")
        );
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
    fn test_auto_cleanup_merged_branches_default_true() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.general.auto_cleanup_merged_branches);
    }

    #[test]
    fn test_auto_cleanup_merged_branches_opt_out() {
        let config: Config = toml::from_str(
            r#"
            [general]
            auto_cleanup_merged_branches = false
        "#,
        )
        .unwrap();
        assert!(!config.general.auto_cleanup_merged_branches);
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

    #[test]
    fn test_load_config_fails_on_malformed_github_app() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Missing required fields (private_key_path, installation_id)
        std::fs::write(
            &path,
            r#"
[github.app]
app_id = 123456
"#,
        )
        .unwrap();
        let result = load_config_from(&path);
        assert!(result.is_err(), "expected Err for malformed [github.app]");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("github.app"),
            "error message should mention github.app, got: {msg}"
        );
    }

    #[test]
    fn test_save_config_preserves_github_app_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Write a valid config with [github.app]
        std::fs::write(
            &path,
            r#"
[github.app]
app_id = 123456
private_key_path = "~/.conductor/conductor-ai.pem"
installation_id = 789012
"#,
        )
        .unwrap();

        // Load succeeds and app is populated
        let mut config = load_config_from(&path).unwrap();
        assert!(config.github.app.is_some());

        // Simulate caller clearing app from memory (the bug scenario)
        config.github.app = None;

        // Save — patch-write should preserve the on-disk [github.app]
        save_config_to(&config, &path).unwrap();

        // Re-read raw TOML and verify [github.app] is still there
        let raw_contents = std::fs::read_to_string(&path).unwrap();
        let raw: toml::Value = toml::from_str(&raw_contents).unwrap();
        assert!(
            raw.get("github").and_then(|g| g.get("app")).is_some(),
            "[github.app] should survive save when app is None in memory"
        );
    }

    #[test]
    fn test_save_config_preserves_notify_hooks_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Write a config with a [[notify.hooks]] entry
        std::fs::write(
            &path,
            r#"
[[notify.hooks]]
on = "workflow_run.*"
url = "https://example.com/hook"
"#,
        )
        .unwrap();

        // Load and verify hook is present
        let config = load_config_from(&path).unwrap();
        assert_eq!(config.notify.hooks.len(), 1);

        // Simulate the bug: save with an in-memory config whose hooks vec is empty (default)
        let empty_hooks_config = Config::default();
        save_config_to(&empty_hooks_config, &path).unwrap();

        // Re-read raw TOML and verify [[notify.hooks]] is still there
        let raw_contents = std::fs::read_to_string(&path).unwrap();
        let raw: toml::Value = toml::from_str(&raw_contents).unwrap();
        assert!(
            raw.get("notify")
                .and_then(|n| n.get("hooks"))
                .and_then(|h| h.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false),
            "[[notify.hooks]] should survive save when hooks vec is empty in memory"
        );
    }

    #[test]
    fn test_save_config_fails_on_malformed_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Write a syntactically invalid TOML file
        std::fs::write(&path, "not valid toml = [ unclosed").unwrap();
        let config = Config::default();
        let result = save_config_to(&config, &path);
        assert!(
            result.is_err(),
            "expected Err when existing config file is malformed"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("malformed"),
            "error message should mention malformed, got: {msg}"
        );
    }

    #[test]
    fn test_github_apps_map_configured() {
        let config: Config = toml::from_str(
            r#"
            [github.apps.developer]
            app_id = 111111
            private_key_path = "~/.conductor/dev-bot.pem"
            installation_id = 222222

            [github.apps.reviewer]
            app_id = 333333
            private_key_path = "~/.conductor/reviewer-bot.pem"
            installation_id = 444444
        "#,
        )
        .unwrap();
        assert_eq!(config.github.apps.len(), 2);
        let dev = config.github.apps.get("developer").unwrap();
        assert_eq!(dev.app_id, 111111);
        let rev = config.github.apps.get("reviewer").unwrap();
        assert_eq!(rev.app_id, 333333);
    }

    #[test]
    fn test_github_apps_and_app_coexist() {
        let config: Config = toml::from_str(
            r#"
            [github.app]
            app_id = 999999
            private_key_path = "~/.conductor/legacy.pem"
            installation_id = 888888

            [github.apps.developer]
            app_id = 111111
            private_key_path = "~/.conductor/dev-bot.pem"
            installation_id = 222222
        "#,
        )
        .unwrap();
        assert!(config.github.app.is_some());
        assert_eq!(config.github.apps.len(), 1);
    }

    #[test]
    fn test_load_config_fails_on_malformed_github_apps_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Missing required fields
        std::fs::write(
            &path,
            r#"
[github.apps.developer]
app_id = 123456
"#,
        )
        .unwrap();
        let result = load_config_from(&path);
        assert!(
            result.is_err(),
            "expected Err for malformed [github.apps.developer]"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("github.apps.developer"),
            "error message should mention github.apps.developer, got: {msg}"
        );
    }

    #[test]
    fn test_notification_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.notifications.enabled);
        assert!(!config.notifications.workflows.on_success);
        assert!(config.notifications.workflows.on_failure);
        assert!(config.notifications.workflows.on_gate_human);
        assert!(!config.notifications.workflows.on_gate_ci);
        assert!(config.notifications.workflows.on_gate_pr_review);
        assert!(config.notifications.slack.webhook_url.is_none());
    }

    #[test]
    fn test_notification_slack_config() {
        let config: Config = toml::from_str(
            r#"
            [notifications]
            enabled = true
            [notifications.slack]
            webhook_url = "https://hooks.slack.com/services/T00/B00/xxx"
        "#,
        )
        .unwrap();
        assert!(config.notifications.enabled);
        assert_eq!(
            config.notifications.slack.webhook_url.as_deref(),
            Some("https://hooks.slack.com/services/T00/B00/xxx")
        );
    }

    #[test]
    fn test_notification_full_override() {
        let config: Config = toml::from_str(
            r#"
            [notifications]
            enabled = true
            [notifications.workflows]
            on_success = true
            on_failure = false
        "#,
        )
        .unwrap();
        assert!(config.notifications.enabled);
        assert!(config.notifications.workflows.on_success);
        assert!(!config.notifications.workflows.on_failure);
        // Gate fields should still be at their defaults
        assert!(config.notifications.workflows.on_gate_human);
        assert!(!config.notifications.workflows.on_gate_ci);
        assert!(config.notifications.workflows.on_gate_pr_review);
    }

    #[test]
    fn test_notification_gate_overrides() {
        let config: Config = toml::from_str(
            r#"
            [notifications]
            enabled = true
            [notifications.workflows]
            on_gate_human = false
            on_gate_ci = true
            on_gate_pr_review = false
        "#,
        )
        .unwrap();
        assert!(!config.notifications.workflows.on_gate_human);
        assert!(config.notifications.workflows.on_gate_ci);
        assert!(!config.notifications.workflows.on_gate_pr_review);
    }

    #[test]
    fn test_save_config_preserves_unknown_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Write a config with an unknown/future top-level section
        std::fs::write(
            &path,
            r#"
[future_feature]
some_key = "some_value"
"#,
        )
        .unwrap();

        // Save a default config on top
        let config = Config::default();
        save_config_to(&config, &path).unwrap();

        // Unknown section should survive
        let raw_contents = std::fs::read_to_string(&path).unwrap();
        let raw: toml::Value = toml::from_str(&raw_contents).unwrap();
        assert!(
            raw.get("future_feature").is_some(),
            "[future_feature] should survive save"
        );
        assert_eq!(
            raw.get("future_feature")
                .and_then(|f| f.get("some_key"))
                .and_then(|v| v.as_str()),
            Some("some_value")
        );
    }

    // -----------------------------------------------------------------------
    // RepoConfig tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_repo_config_load_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let rc = RepoConfig::load(dir.path()).unwrap();
        assert!(rc.defaults.model.is_none());
        assert!(rc.defaults.default_branch.is_none());
        assert!(rc.defaults.bot_name.is_none());
        assert!(rc.defaults.feature_merge_strategy.is_none());
    }

    #[test]
    fn test_repo_config_load_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let conductor_dir = dir.path().join(".conductor");
        std::fs::create_dir_all(&conductor_dir).unwrap();
        std::fs::write(
            conductor_dir.join("config.toml"),
            r#"
[defaults]
model = "claude-opus-4-6"
default_branch = "develop"
"#,
        )
        .unwrap();

        let rc = RepoConfig::load(dir.path()).unwrap();
        assert_eq!(rc.defaults.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(rc.defaults.default_branch.as_deref(), Some("develop"));
        assert!(rc.defaults.bot_name.is_none());
    }

    #[test]
    fn test_repo_config_load_partial_fields() {
        let dir = tempfile::tempdir().unwrap();
        let conductor_dir = dir.path().join(".conductor");
        std::fs::create_dir_all(&conductor_dir).unwrap();
        std::fs::write(
            conductor_dir.join("config.toml"),
            r#"
[defaults]
bot_name = "my-bot"
"#,
        )
        .unwrap();

        let rc = RepoConfig::load(dir.path()).unwrap();
        assert!(rc.defaults.model.is_none());
        assert!(rc.defaults.default_branch.is_none());
        assert_eq!(rc.defaults.bot_name.as_deref(), Some("my-bot"));
    }

    #[test]
    fn test_repo_config_save_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let rc = RepoConfig {
            defaults: RepoDefaults {
                model: Some("sonnet".to_string()),
                default_branch: Some("main".to_string()),
                bot_name: None,
                feature_merge_strategy: Some("merge".to_string()),
            },
        };
        rc.save(dir.path()).unwrap();

        let loaded = RepoConfig::load(dir.path()).unwrap();
        assert_eq!(loaded.defaults.model.as_deref(), Some("sonnet"));
        assert_eq!(loaded.defaults.default_branch.as_deref(), Some("main"));
        assert!(loaded.defaults.bot_name.is_none());
        assert_eq!(
            loaded.defaults.feature_merge_strategy.as_deref(),
            Some("merge")
        );
    }

    #[test]
    fn test_repo_config_save_creates_conductor_dir() {
        let dir = tempfile::tempdir().unwrap();
        let rc = RepoConfig::default();
        rc.save(dir.path()).unwrap();
        assert!(dir.path().join(".conductor").join("config.toml").exists());
    }

    #[test]
    fn test_repo_config_save_clears_option_fields() {
        let dir = tempfile::tempdir().unwrap();
        // First, save a config with model set.
        let rc = RepoConfig {
            defaults: RepoDefaults {
                model: Some("opus".to_string()),
                default_branch: Some("develop".to_string()),
                bot_name: None,
                feature_merge_strategy: None,
            },
        };
        rc.save(dir.path()).unwrap();
        let loaded = RepoConfig::load(dir.path()).unwrap();
        assert_eq!(loaded.defaults.model.as_deref(), Some("opus"));
        assert_eq!(loaded.defaults.default_branch.as_deref(), Some("develop"));

        // Now clear model by saving with None — it must actually be removed.
        let rc2 = RepoConfig {
            defaults: RepoDefaults {
                model: None,
                default_branch: Some("develop".to_string()),
                bot_name: None,
                feature_merge_strategy: None,
            },
        };
        rc2.save(dir.path()).unwrap();
        let loaded2 = RepoConfig::load(dir.path()).unwrap();
        assert!(
            loaded2.defaults.model.is_none(),
            "model should be cleared after saving with None"
        );
        assert_eq!(loaded2.defaults.default_branch.as_deref(), Some("develop"));
    }

    #[test]
    fn test_db_path_env_override() {
        let _guard = DB_PATH_ENV_LOCK.lock().unwrap();
        let custom = "/tmp/conductor-test-db-path-override.db";
        unsafe {
            std::env::set_var("CONDUCTOR_DB_PATH", custom);
        }
        let result = db_path();
        unsafe {
            std::env::remove_var("CONDUCTOR_DB_PATH");
        }
        assert_eq!(result, PathBuf::from(custom));
    }

    // -----------------------------------------------------------------------
    // resolved_claude_config_dir / custom_claude_config_dir tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolved_claude_config_dir_none_falls_back_to_home_claude() {
        let config = GeneralConfig {
            claude_config_dir: None,
            ..GeneralConfig::default()
        };
        let result = config.resolved_claude_config_dir().unwrap();
        // Should be <home>/.claude
        let expected = dirs::home_dir().unwrap().join(".claude");
        assert_eq!(result, expected);
    }

    #[test]
    fn test_resolved_claude_config_dir_absolute_path() {
        let config = GeneralConfig {
            claude_config_dir: Some("/tmp/my-claude".to_string()),
            ..GeneralConfig::default()
        };
        let result = config.resolved_claude_config_dir().unwrap();
        assert_eq!(result, PathBuf::from("/tmp/my-claude"));
    }

    #[test]
    fn test_resolved_claude_config_dir_expands_tilde() {
        let config = GeneralConfig {
            claude_config_dir: Some("~/.claude-personal".to_string()),
            ..GeneralConfig::default()
        };
        let result = config.resolved_claude_config_dir().unwrap();
        let expected = dirs::home_dir().unwrap().join(".claude-personal");
        assert_eq!(result, expected);
    }

    #[test]
    fn test_custom_claude_config_dir_none_when_not_configured() {
        let config = GeneralConfig {
            claude_config_dir: None,
            ..GeneralConfig::default()
        };
        assert!(config.custom_claude_config_dir().is_none());
    }

    #[test]
    fn test_custom_claude_config_dir_some_ok_when_configured() {
        let config = GeneralConfig {
            claude_config_dir: Some("/tmp/custom-claude".to_string()),
            ..GeneralConfig::default()
        };
        let result = config.custom_claude_config_dir();
        assert!(result.is_some());
        let path = result.unwrap().unwrap();
        assert_eq!(path, PathBuf::from("/tmp/custom-claude"));
    }

    #[test]
    fn test_custom_claude_config_dir_some_ok_expands_tilde() {
        let config = GeneralConfig {
            claude_config_dir: Some("~/.claude-custom".to_string()),
            ..GeneralConfig::default()
        };
        let result = config.custom_claude_config_dir().unwrap().unwrap();
        let expected = dirs::home_dir().unwrap().join(".claude-custom");
        assert_eq!(result, expected);
    }

    // -----------------------------------------------------------------------
    // resolve_optional_claude_dir tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_optional_claude_dir_none_when_not_configured() {
        let config = GeneralConfig {
            claude_config_dir: None,
            ..GeneralConfig::default()
        };
        // Not configured → None (no fallback to ~/.claude)
        assert!(config.resolve_optional_claude_dir().is_none());
    }

    #[test]
    fn test_resolve_optional_claude_dir_some_for_absolute_path() {
        let config = GeneralConfig {
            claude_config_dir: Some("/tmp/my-claude".to_string()),
            ..GeneralConfig::default()
        };
        let result = config.resolve_optional_claude_dir();
        assert_eq!(result, Some(PathBuf::from("/tmp/my-claude")));
    }

    #[test]
    fn test_resolve_optional_claude_dir_expands_tilde() {
        let config = GeneralConfig {
            claude_config_dir: Some("~/.claude-personal".to_string()),
            ..GeneralConfig::default()
        };
        let result = config.resolve_optional_claude_dir();
        let expected = dirs::home_dir().unwrap().join(".claude-personal");
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn test_db_path_empty_env_falls_back_to_default() {
        let _guard = DB_PATH_ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("CONDUCTOR_DB_PATH", "");
        }
        let result = db_path();
        unsafe {
            std::env::remove_var("CONDUCTOR_DB_PATH");
        }
        // Should fall back to conductor_dir()/conductor.db
        assert_eq!(result, conductor_dir().join("conductor.db"));
    }
}
