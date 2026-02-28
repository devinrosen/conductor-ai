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

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            workspace_root: default_workspace_root(),
            sync_interval_minutes: default_sync_interval(),
            editor: None,
            work_targets: default_work_targets(),
            auto_start_agent: AutoStartAgent::default(),
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
}
