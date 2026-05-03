use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// TUI-specific configuration stored under `[tui]` in `~/.conductor/config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TuiConfig {
    /// TUI color theme. One of the built-in names or the stem of a custom file in
    /// `~/.conductor/themes/`. Omit to use the default conductor theme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

/// Returns the directory for user-supplied theme files: `~/.conductor/themes/`
pub fn themes_dir() -> PathBuf {
    conductor_core::config::conductor_dir().join("themes")
}

/// Ensure TUI-specific data directories exist. Call at startup before any theme loading.
pub fn ensure_tui_dirs() -> Result<()> {
    std::fs::create_dir_all(themes_dir())?;
    Ok(())
}

/// Load TUI config from `~/.conductor/config.toml`, reading the `[tui]` section.
///
/// Falls back to `[general].theme` when `[tui].theme` is absent (legacy migration path).
pub fn load_tui_config() -> Result<TuiConfig> {
    load_from(&conductor_core::config::config_path())
}

/// Patch-write only the `[tui]` section into `~/.conductor/config.toml`, preserving all
/// other sections.
pub fn save_tui_config(cfg: &TuiConfig) -> Result<()> {
    save_to(cfg, &conductor_core::config::config_path())
}

fn load_from(path: &Path) -> Result<TuiConfig> {
    if !path.exists() {
        return Ok(TuiConfig::default());
    }
    let contents = std::fs::read_to_string(path)?;
    let raw: toml::Value = toml::from_str(&contents)?;

    let mut cfg: TuiConfig = if let Some(tui_section) = raw.get("tui") {
        tui_section.clone().try_into().unwrap_or_default()
    } else {
        TuiConfig::default()
    };

    // Legacy fallback: if [tui].theme is absent but [general].theme is present, use it.
    if cfg.theme.is_none() {
        if let Some(legacy) = raw
            .get("general")
            .and_then(|g| g.get("theme"))
            .and_then(|t| t.as_str())
        {
            tracing::warn!(
                "[general].theme is deprecated — move to [tui].theme; conductor-core no longer reads it"
            );
            cfg.theme = Some(legacy.to_string());
        }
    }

    Ok(cfg)
}

fn save_to(cfg: &TuiConfig, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut merged: toml::Value = if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        toml::from_str(&existing)
            .map_err(|e| anyhow::anyhow!("existing config is malformed: {e}"))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    let tui_value: toml::Value =
        toml::Value::try_from(cfg).map_err(|e| anyhow::anyhow!("serialize tui config: {e}"))?;

    if let toml::Value::Table(ref mut table) = merged {
        table.insert("tui".to_string(), tui_value);
    }

    let contents = toml::to_string_pretty(&merged)
        .map_err(|e| anyhow::anyhow!("serialize tui config: {e}"))?;
    std::fs::write(path, contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_load_reads_tui_section() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[tui]\ntheme = \"nord\"\n").unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("nord"));
    }

    #[test]
    fn test_load_legacy_general_theme_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[general]\ntheme = \"gruvbox\"\n").unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("gruvbox"));
    }

    #[test]
    fn test_load_tui_theme_takes_precedence_over_legacy() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[general]\ntheme = \"gruvbox\"\n\n[tui]\ntheme = \"nord\"\n",
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("nord"));
    }

    #[test]
    fn test_load_defaults_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // File doesn't exist — should return default without error.
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.theme, None);
    }

    #[test]
    fn test_save_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = TuiConfig {
            theme: Some("nord".to_string()),
        };
        save_to(&cfg, &path).unwrap();
        let reloaded = load_from(&path).unwrap();
        assert_eq!(reloaded.theme.as_deref(), Some("nord"));
    }

    #[test]
    fn test_save_preserves_unknown_sections() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[general]\nsync_interval_minutes = 30\n\n[github]\ntoken = \"secret\"\n\n[future_feature]\nsomething = true\n",
        )
        .unwrap();

        let cfg = TuiConfig {
            theme: Some("catppuccin_mocha".to_string()),
        };
        save_to(&cfg, &path).unwrap();

        let reloaded = load_from(&path).unwrap();
        assert_eq!(reloaded.theme.as_deref(), Some("catppuccin_mocha"));

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("[future_feature]"),
            "future_feature section should be preserved"
        );
        assert!(
            contents.contains("[github]"),
            "github section should be preserved"
        );
        assert!(
            contents.contains("sync_interval_minutes"),
            "general section should be preserved"
        );
    }

    #[test]
    fn test_save_preserves_general_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[general]\nsync_interval_minutes = 45\n").unwrap();

        let cfg = TuiConfig {
            theme: Some("gruvbox".to_string()),
        };
        save_to(&cfg, &path).unwrap();

        let raw: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            raw.get("general")
                .and_then(|g| g.get("sync_interval_minutes"))
                .and_then(|v| v.as_integer()),
            Some(45),
            "[general].sync_interval_minutes should be unchanged after saving [tui]"
        );
    }
}
