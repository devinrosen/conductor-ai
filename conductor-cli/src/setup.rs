use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// Read and parse a JSON settings file, returning `Value::Object({})` if the
/// file does not exist.
fn read_settings(path: &Path) -> Result<serde_json::Value> {
    let raw = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        "{}".to_string()
    };
    let value: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("invalid JSON in {}", path.display()))?;
    Ok(value)
}

/// Serialize `value` and write it to `path`.
fn write_settings(path: &Path, value: &serde_json::Value) -> Result<()> {
    let updated =
        serde_json::to_string_pretty(value).context("failed to serialize settings.json")?;
    fs::write(path, updated).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Core install logic, parameterised for testability.
fn install_to(settings_path: &Path) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut value = read_settings(settings_path)?;

    if !value.is_object() {
        anyhow::bail!(
            "{} contains valid JSON but is not an object; refusing to modify it",
            settings_path.display()
        );
    }

    // Set mcpServers.conductor
    {
        let mcp_servers = value["mcpServers"]
            .as_object_mut()
            .cloned()
            .unwrap_or_default();
        let mut mcp_servers = mcp_servers;
        mcp_servers.insert(
            "conductor".to_string(),
            serde_json::json!({
                "command": "conductor",
                "args": ["mcp", "serve"]
            }),
        );
        value["mcpServers"] = serde_json::Value::Object(mcp_servers);
    }

    write_settings(settings_path, &value)?;

    println!("Conductor MCP server registered in Claude Code.");
    println!(
        "  Setting: {} → mcpServers.conductor",
        settings_path.display()
    );
    println!();
    println!("Restart Claude Code to activate the MCP server.");

    Ok(())
}

/// Core uninstall logic, parameterised for testability.
fn uninstall_from(settings_path: &Path) -> Result<()> {
    if !settings_path.exists() {
        println!(
            "Nothing to uninstall: {} not found.",
            settings_path.display()
        );
        return Ok(());
    }

    let mut value = read_settings(settings_path)?;

    let has_mcp = value
        .get("mcpServers")
        .and_then(|v| v.get("conductor"))
        .is_some();

    if !has_mcp {
        println!(
            "Nothing to uninstall: conductor keys not found in {}.",
            settings_path.display()
        );
        return Ok(());
    }

    // Remove mcpServers.conductor; drop the whole mcpServers key if empty.
    if let Some(mcp_obj) = value["mcpServers"].as_object_mut() {
        mcp_obj.remove("conductor");
        if mcp_obj.is_empty() {
            if let Some(obj) = value.as_object_mut() {
                obj.remove("mcpServers");
            }
        }
    }

    write_settings(settings_path, &value)?;

    println!("Conductor MCP server unregistered.");
    println!("  Removed conductor keys from {}", settings_path.display());

    Ok(())
}

fn claude_settings_path() -> Result<std::path::PathBuf> {
    let config = conductor_core::config::load_config()?;
    Ok(config
        .general
        .resolved_claude_config_dir()?
        .join("settings.json"))
}

/// Register the conductor MCP server in Claude Code's settings.json.
///
/// Updates `<claude_config_dir>/settings.json` to add `mcpServers.conductor`.
/// The Claude config directory is read from conductor's config (`claude_config_dir`),
/// defaulting to `~/.claude` when unset.
pub fn install() -> Result<()> {
    install_to(&claude_settings_path()?)
}

/// Unregister the conductor MCP server from Claude Code's settings.json.
///
/// Removes `mcpServers.conductor` from `<claude_config_dir>/settings.json`.
pub fn uninstall() -> Result<()> {
    uninstall_from(&claude_settings_path()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("conductor_setup_test_{pid}_{n}"));
        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap();
        }
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn install_sets_mcp_server() {
        let root = temp_dir();
        let settings = root.join("settings.json");

        install_to(&settings).unwrap();

        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            v["mcpServers"]["conductor"]["command"].as_str().unwrap(),
            "conductor"
        );
        assert_eq!(
            v["mcpServers"]["conductor"]["args"][0].as_str().unwrap(),
            "mcp"
        );
        assert_eq!(
            v["mcpServers"]["conductor"]["args"][1].as_str().unwrap(),
            "serve"
        );
    }

    #[test]
    fn install_preserves_existing_settings() {
        let root = temp_dir();
        let settings = root.join("settings.json");

        fs::write(&settings, r#"{"someOtherKey": true}"#).unwrap();

        install_to(&settings).unwrap();

        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v["someOtherKey"].as_bool().unwrap());
        assert!(v["mcpServers"]["conductor"].is_object());
    }

    #[test]
    fn install_rejects_non_object_settings_json() {
        let root = temp_dir();
        let settings = root.join("settings.json");

        fs::write(&settings, r#"["not", "an", "object"]"#).unwrap();

        let err = install_to(&settings).unwrap_err();
        assert!(
            err.to_string().contains("not an object"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn install_rejects_invalid_json_in_settings() {
        let root = temp_dir();
        let settings = root.join("settings.json");

        fs::write(&settings, "not json at all").unwrap();

        let err = install_to(&settings).unwrap_err();
        assert!(
            err.to_string().contains("invalid JSON"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn uninstall_removes_mcp_server() {
        let root = temp_dir();
        let settings = root.join("settings.json");

        install_to(&settings).unwrap();
        uninstall_from(&settings).unwrap();

        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(
            v.get("mcpServers").is_none(),
            "mcpServers should have been removed (was empty)"
        );
    }

    #[test]
    fn uninstall_leaves_other_mcp_servers_untouched() {
        let root = temp_dir();
        let settings = root.join("settings.json");

        // Pre-populate with another MCP server
        fs::write(
            &settings,
            r#"{"mcpServers": {"other-server": {"command": "other"}}}"#,
        )
        .unwrap();

        install_to(&settings).unwrap();

        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v["mcpServers"]["conductor"].is_object());
        assert!(v["mcpServers"]["other-server"].is_object());

        uninstall_from(&settings).unwrap();

        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(
            v.get("mcpServers")
                .and_then(|m| m.get("conductor"))
                .is_none(),
            "mcpServers.conductor should be removed"
        );
        assert!(
            v["mcpServers"]["other-server"].is_object(),
            "other-server should remain"
        );
    }

    #[test]
    fn uninstall_is_idempotent_when_key_absent() {
        let root = temp_dir();
        let settings = root.join("settings.json");
        fs::write(&settings, r#"{"otherKey": 1}"#).unwrap();

        uninstall_from(&settings).unwrap();

        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["otherKey"].as_i64().unwrap(), 1);
    }

    #[test]
    fn uninstall_succeeds_when_settings_file_absent() {
        let root = temp_dir();
        let settings = root.join("nonexistent_settings.json");

        uninstall_from(&settings).unwrap();
    }

    #[test]
    fn install_then_reinstall_is_idempotent() {
        let root = temp_dir();
        let settings = root.join("settings.json");

        install_to(&settings).unwrap();
        install_to(&settings).unwrap();

        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v["mcpServers"]["conductor"].is_object());
    }
}
