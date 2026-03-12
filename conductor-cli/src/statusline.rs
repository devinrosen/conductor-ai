use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// The statusline Python script embedded at compile time.
static STATUSLINE_SCRIPT: &str = include_str!("../../scripts/statusline.py");

fn conductor_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".conductor"))
}

fn claude_settings_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".claude").join("settings.json"))
}

/// Install the conductor status line into Claude Code.
///
/// Writes `~/.conductor/statusline.py`, marks it executable, then updates
/// `~/.claude/settings.json` to set `statusLineTool` to that path.
pub fn install() -> Result<()> {
    // 1. Write the embedded script to ~/.conductor/statusline.py
    let conductor_dir = conductor_dir()?;
    fs::create_dir_all(&conductor_dir)
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

    // 3. Read (or create) ~/.claude/settings.json
    let settings_path = claude_settings_path()?;
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let existing = if settings_path.exists() {
        fs::read_to_string(&settings_path)
            .with_context(|| format!("failed to read {}", settings_path.display()))?
    } else {
        "{}".to_string()
    };

    let mut value: serde_json::Value = serde_json::from_str(&existing)
        .with_context(|| format!("invalid JSON in {}", settings_path.display()))?;

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

    // 5. Write settings back
    let updated =
        serde_json::to_string_pretty(&value).context("failed to serialize settings.json")?;
    fs::write(&settings_path, updated)
        .with_context(|| format!("failed to write {}", settings_path.display()))?;

    println!("Conductor status line installed.");
    println!("  Script:  {}", script_path.display());
    println!("  Setting: {} → statusLineTool", settings_path.display());
    println!();
    println!("Restart Claude Code to activate the status line.");

    Ok(())
}

/// Uninstall the conductor status line from Claude Code.
///
/// Removes `statusLineTool` from `~/.claude/settings.json`.
/// Leaves `~/.conductor/statusline.py` in place for fast reinstall.
pub fn uninstall() -> Result<()> {
    let settings_path = claude_settings_path()?;

    if !settings_path.exists() {
        println!(
            "Nothing to uninstall: {} not found.",
            settings_path.display()
        );
        return Ok(());
    }

    let existing = fs::read_to_string(&settings_path)
        .with_context(|| format!("failed to read {}", settings_path.display()))?;

    let mut value: serde_json::Value = serde_json::from_str(&existing)
        .with_context(|| format!("invalid JSON in {}", settings_path.display()))?;

    if value.get("statusLineTool").is_none() {
        println!("statusLineTool is not set in {}.", settings_path.display());
        return Ok(());
    }

    if let Some(obj) = value.as_object_mut() {
        obj.remove("statusLineTool");
    }

    let updated =
        serde_json::to_string_pretty(&value).context("failed to serialize settings.json")?;
    fs::write(&settings_path, updated)
        .with_context(|| format!("failed to write {}", settings_path.display()))?;

    println!("Conductor status line uninstalled.");
    println!("  Removed statusLineTool from {}", settings_path.display());

    Ok(())
}
