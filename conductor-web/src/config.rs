use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// VAPID keys and subject for web push notifications, stored under `[web].push`
/// in `~/.conductor/config.toml`.
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

/// Web-specific configuration stored under `[web]` in `~/.conductor/config.toml`.
///
/// Mirrors the `[tui]` parent-section pattern from #2679/#2838.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebConfig {
    #[serde(default)]
    pub push: WebPushConfig,
}

/// Load web config from `~/.conductor/config.toml`, reading the `[web]` section.
///
/// Falls back to top-level `[web_push]` when `[web]` is absent (legacy migration path).
/// The deprecation warn for `[web_push]` is emitted by `conductor-core::load_config_from`.
pub fn load_web_config() -> Result<WebConfig> {
    load_from(&conductor_core::config::config_path())
}

/// Patch-write only the `[web]` section into `~/.conductor/config.toml`, preserving all
/// other sections.
pub fn save_web_config(cfg: &WebConfig) -> Result<()> {
    save_to(cfg, &conductor_core::config::config_path())
}

fn load_from(path: &Path) -> Result<WebConfig> {
    if !path.exists() {
        return Ok(WebConfig::default());
    }
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let raw: toml::Value =
        toml::from_str(&contents).with_context(|| format!("parse {}", path.display()))?;

    if let Some(web_section) = raw.get("web") {
        match web_section.clone().try_into::<WebConfig>() {
            Ok(cfg) => return Ok(cfg),
            Err(e) => {
                tracing::warn!(
                    "ignoring malformed [web] section in {}: {e}",
                    path.display()
                );
                // Fall through to legacy fallback below.
            }
        }
    }

    // Legacy fallback: top-level [web_push] section (deprecated).
    // conductor-core::load_config_from emits the deprecation warn — no duplicate here.
    if let Some(legacy) = raw.get("web_push") {
        if let Ok(push_cfg) = legacy.clone().try_into::<WebPushConfig>() {
            return Ok(WebConfig { push: push_cfg });
        }
    }

    Ok(WebConfig::default())
}

fn save_to(cfg: &WebConfig, path: &Path) -> Result<()> {
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

    let web_value: toml::Value = toml::Value::try_from(cfg)
        .with_context(|| format!("serialize web config for {}", path.display()))?;

    if let toml::Value::Table(ref mut table) = merged {
        table.insert("web".to_string(), web_value);
    }

    let contents = toml::to_string_pretty(&merged)
        .with_context(|| format!("serialize merged config for {}", path.display()))?;
    std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_load_reads_web_push_section() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[web]\n[web.push]\nvapid_public_key = \"test_pub\"\nvapid_private_key = \"test_priv\"\nvapid_subject = \"mailto:test@example.com\"\n",
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.push.vapid_public_key.as_deref(), Some("test_pub"));
        assert_eq!(cfg.push.vapid_private_key.as_deref(), Some("test_priv"));
        assert_eq!(
            cfg.push.vapid_subject.as_deref(),
            Some("mailto:test@example.com")
        );
    }

    #[test]
    fn test_load_legacy_web_push_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[web_push]\nvapid_public_key = \"legacy_pub\"\nvapid_subject = \"mailto:legacy@example.com\"\n",
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.push.vapid_public_key.as_deref(), Some("legacy_pub"));
        assert_eq!(
            cfg.push.vapid_subject.as_deref(),
            Some("mailto:legacy@example.com")
        );
    }

    #[test]
    fn test_load_web_takes_precedence_over_legacy() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[web_push]\nvapid_public_key = \"legacy_pub\"\n\n[web]\n[web.push]\nvapid_public_key = \"new_pub\"\n",
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(
            cfg.push.vapid_public_key.as_deref(),
            Some("new_pub"),
            "[web].push should take precedence over [web_push]"
        );
    }

    #[test]
    fn test_load_malformed_web_section_falls_back_to_default() {
        // [web].push.vapid_public_key = 42 is type-mismatched (expects Option<String>).
        // Loader must log and fall back to default rather than failing outright.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[web]\n[web.push]\nvapid_public_key = 42\n").unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(
            cfg.push.vapid_public_key, None,
            "malformed [web] section must fall back to default"
        );
    }

    #[test]
    fn test_load_malformed_web_falls_back_to_legacy() {
        // When [web] is malformed but [web_push] is valid, legacy fallback must recover.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[web_push]\nvapid_public_key = \"legacy_pub\"\n\n[web]\n[web.push]\nvapid_public_key = 42\n",
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.push.vapid_public_key.as_deref(), Some("legacy_pub"));
    }

    #[test]
    fn test_save_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = WebConfig {
            push: WebPushConfig {
                vapid_public_key: Some("pub_key".to_string()),
                vapid_private_key: Some("priv_key".to_string()),
                vapid_subject: Some("mailto:test@example.com".to_string()),
            },
        };
        save_to(&cfg, &path).unwrap();
        let reloaded = load_from(&path).unwrap();
        assert_eq!(reloaded.push.vapid_public_key.as_deref(), Some("pub_key"));
        assert_eq!(reloaded.push.vapid_private_key.as_deref(), Some("priv_key"));
        assert_eq!(
            reloaded.push.vapid_subject.as_deref(),
            Some("mailto:test@example.com")
        );
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

        let cfg = WebConfig {
            push: WebPushConfig {
                vapid_public_key: Some("pub".to_string()),
                vapid_private_key: None,
                vapid_subject: None,
            },
        };
        save_to(&cfg, &path).unwrap();

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
        let reloaded = load_from(&path).unwrap();
        assert_eq!(reloaded.push.vapid_public_key.as_deref(), Some("pub"));
    }

    #[test]
    fn test_save_preserves_general_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[general]\nsync_interval_minutes = 45\n").unwrap();

        let cfg = WebConfig {
            push: WebPushConfig {
                vapid_public_key: Some("pub".to_string()),
                vapid_private_key: Some("priv".to_string()),
                vapid_subject: Some("mailto:test@example.com".to_string()),
            },
        };
        save_to(&cfg, &path).unwrap();

        let raw: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            raw.get("general")
                .and_then(|g| g.get("sync_interval_minutes"))
                .and_then(|v| v.as_integer()),
            Some(45),
            "[general].sync_interval_minutes should be unchanged after saving [web]"
        );
    }
}
