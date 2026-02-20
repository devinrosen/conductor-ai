use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::error::{ConductorError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            defaults: DefaultsConfig::default(),
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
pub fn load_config() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = std::fs::read_to_string(&path)?;
    toml::from_str(&contents).map_err(|e| ConductorError::Config(e.to_string()))
}

/// Ensure the conductor data directory exists.
pub fn ensure_dirs(config: &Config) -> Result<()> {
    std::fs::create_dir_all(conductor_dir())?;
    std::fs::create_dir_all(&config.general.workspace_root)?;
    Ok(())
}
