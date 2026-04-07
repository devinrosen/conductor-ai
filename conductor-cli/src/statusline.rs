use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The statusline Python script embedded at compile time.
static STATUSLINE_SCRIPT: &str = include_str!("../../scripts/statusline.py");

fn conductor_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".conductor"))
}

/// Read and parse `~/.claude/settings.json` (or `path`), returning the JSON
/// value. Returns `Value::Object({})` if the file does not exist.
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
fn install_to(conductor_dir: &Path, settings_path: &Path) -> Result<()> {
    // 1. Write the embedded script to <conductor_dir>/statusline.py
    fs::create_dir_all(conductor_dir)
        .with_context(|| format!("failed to create {}", conductor_dir.display()))?;

    let script_path = conductor_dir.join("statusline.py");
    fs::write(&script_path, STATUSLINE_SCRIPT)
        .with_context(|| format!("failed to write {}", script_path.display()))?;

    // 2. chmod +x
    let mut perms = fs::metadata(&script_path)
        .with_context(|| format!("cannot stat {}", script_path.display()))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)
        .with_context(|| format!("failed to chmod {}", script_path.display()))?;

    // 3. Read (or create) settings.json
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

    // 4. Set statusLineTool
    let script_path_str = script_path
        .to_str()
        .context("script path is not valid UTF-8")?;
    value["statusLineTool"] = serde_json::Value::String(script_path_str.to_string());

    // 5. Set mcpServers.conductor
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

    // 6. Write settings back
    write_settings(settings_path, &value)?;

    println!("Conductor status line installed.");
    println!("  Script:  {}", script_path.display());
    println!("  Setting: {} → statusLineTool", settings_path.display());
    println!(
        "  Setting: {} → mcpServers.conductor",
        settings_path.display()
    );
    println!();
    println!("Restart Claude Code to activate the status line and MCP server.");

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

    let has_statusline = value.get("statusLineTool").is_some();
    let has_mcp = value
        .get("mcpServers")
        .and_then(|v| v.get("conductor"))
        .is_some();

    if !has_statusline && !has_mcp {
        println!(
            "Nothing to uninstall: conductor keys not found in {}.",
            settings_path.display()
        );
        return Ok(());
    }

    if let Some(obj) = value.as_object_mut() {
        obj.remove("statusLineTool");
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

    println!("Conductor status line uninstalled.");
    println!("  Removed conductor keys from {}", settings_path.display());

    Ok(())
}

/// Install the conductor status line into Claude Code.
///
/// Writes `~/.conductor/statusline.py`, marks it executable, then updates
/// `<claude_config_dir>/settings.json` to set `statusLineTool` to that path.
/// The Claude config directory is read from conductor's config (`claude_config_dir`),
/// defaulting to `~/.claude` when unset.
pub fn install() -> Result<()> {
    let config = conductor_core::config::load_config()?;
    let claude_dir = config.general.resolved_claude_config_dir()?;
    install_to(&conductor_dir()?, &claude_dir.join("settings.json"))
}

/// Uninstall the conductor status line from Claude Code.
///
/// Removes `statusLineTool` from `<claude_config_dir>/settings.json`.
/// Leaves `~/.conductor/statusline.py` in place for fast reinstall.
pub fn uninstall() -> Result<()> {
    let config = conductor_core::config::load_config()?;
    let claude_dir = config.general.resolved_claude_config_dir()?;
    uninstall_from(&claude_dir.join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("conductor_statusline_test_{n}"));
        // Remove any stale directory from a prior test run so tests never see
        // leftover files (e.g. invalid JSON from install_rejects_invalid_json_in_settings).
        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap();
        }
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn install_creates_script_and_sets_statusline_tool() {
        let root = temp_dir();
        let conductor = root.join("conductor");
        let settings = root.join("settings.json");

        install_to(&conductor, &settings).unwrap();

        // Script should exist and be executable
        let script = conductor.join("statusline.py");
        assert!(script.exists(), "statusline.py was not created");
        let mode = fs::metadata(&script).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "statusline.py is not executable");

        // settings.json should have statusLineTool set to the script path
        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            v["statusLineTool"].as_str().unwrap(),
            script.to_str().unwrap()
        );
        // mcpServers.conductor should be set
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
    fn install_updates_existing_settings_json() {
        let root = temp_dir();
        let conductor = root.join("conductor");
        let settings = root.join("settings.json");

        // Pre-existing settings with another key
        fs::write(&settings, r#"{"someOtherKey": true}"#).unwrap();

        install_to(&conductor, &settings).unwrap();

        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        // Original key preserved
        assert!(v["someOtherKey"].as_bool().unwrap());
        // statusLineTool set
        assert!(v["statusLineTool"].is_string());
    }

    #[test]
    fn install_rejects_non_object_settings_json() {
        let root = temp_dir();
        let conductor = root.join("conductor");
        let settings = root.join("settings.json");

        fs::write(&settings, r#"["not", "an", "object"]"#).unwrap();

        let err = install_to(&conductor, &settings).unwrap_err();
        assert!(
            err.to_string().contains("not an object"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn install_rejects_invalid_json_in_settings() {
        let root = temp_dir();
        let conductor = root.join("conductor");
        let settings = root.join("settings.json");

        fs::write(&settings, "not json at all").unwrap();

        let err = install_to(&conductor, &settings).unwrap_err();
        assert!(
            err.to_string().contains("invalid JSON"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn uninstall_removes_statusline_tool_and_mcp_servers() {
        let root = temp_dir();
        let conductor = root.join("conductor");
        let settings = root.join("settings.json");

        // Install first
        install_to(&conductor, &settings).unwrap();

        // Now uninstall
        uninstall_from(&settings).unwrap();

        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(
            v.get("statusLineTool").is_none(),
            "statusLineTool should have been removed"
        );
        assert!(
            v.get("mcpServers").is_none(),
            "mcpServers should have been removed (was empty)"
        );
    }

    #[test]
    fn uninstall_leaves_other_mcp_servers_untouched() {
        let root = temp_dir();
        let conductor = root.join("conductor");
        let settings = root.join("settings.json");

        // Pre-populate with another MCP server
        fs::write(
            &settings,
            r#"{"mcpServers": {"other-server": {"command": "other"}}}"#,
        )
        .unwrap();

        // Install (should add conductor entry)
        install_to(&conductor, &settings).unwrap();

        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v["mcpServers"]["conductor"].is_object());
        assert!(v["mcpServers"]["other-server"].is_object());

        // Uninstall (should remove conductor but leave other-server)
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

        // Should succeed without error even though conductor keys are not set
        uninstall_from(&settings).unwrap();

        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["otherKey"].as_i64().unwrap(), 1);
    }

    #[test]
    fn uninstall_succeeds_when_settings_file_absent() {
        let root = temp_dir();
        let settings = root.join("nonexistent_settings.json");

        // Should succeed without error
        uninstall_from(&settings).unwrap();
    }

    #[test]
    fn install_then_reinstall_updates_script() {
        let root = temp_dir();
        let conductor = root.join("conductor");
        let settings = root.join("settings.json");

        install_to(&conductor, &settings).unwrap();
        // Second install should succeed (idempotent)
        install_to(&conductor, &settings).unwrap();

        let raw = fs::read_to_string(&settings).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v["statusLineTool"].is_string());
    }
}
