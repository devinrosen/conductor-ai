use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
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
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let raw: toml::Value =
        toml::from_str(&contents).with_context(|| format!("parse {}", path.display()))?;

    let mut cfg: TuiConfig = if let Some(tui_section) = raw.get("tui") {
        match tui_section.clone().try_into() {
            Ok(parsed) => parsed,
            Err(e) => {
                tracing::warn!(
                    "ignoring malformed [tui] section in {}: {e}",
                    path.display()
                );
                TuiConfig::default()
            }
        }
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
            // conductor-core already emits a deprecation warn for [general].theme —
            // no need to duplicate it here.
            cfg.theme = Some(legacy.to_string());
        }
    }

    Ok(cfg)
}

fn save_to(cfg: &TuiConfig, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {}", parent.display()))?;
    }

    let mut merged: toml::Value = if path.exists() {
        let existing =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        toml::from_str(&existing)
            .with_context(|| format!("existing config is malformed: {}", path.display()))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    let tui_value: toml::Value = toml::Value::try_from(cfg).context("serialize tui config")?;

    if let toml::Value::Table(ref mut table) = merged {
        table.insert("tui".to_string(), tui_value);
    }

    let contents = toml::to_string_pretty(&merged).context("serialize tui config")?;
    std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))?;
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
    fn test_load_malformed_tui_section_falls_back_to_default() {
        // [tui].theme = 42 is type-mismatched (expects Option<String>). Loader must
        // log and fall back to default rather than swallow silently or fail outright,
        // so user misconfigurations are visible without bricking the TUI.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[tui]\ntheme = 42\n").unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(
            cfg.theme, None,
            "malformed [tui] section must fall back to default"
        );
    }

    #[test]
    fn test_load_malformed_tui_falls_back_to_legacy() {
        // When [tui] is malformed but [general].theme is valid, fallback should
        // still recover the legacy theme rather than ignore both.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[general]\ntheme = \"gruvbox\"\n\n[tui]\ntheme = 42\n",
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("gruvbox"));
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
