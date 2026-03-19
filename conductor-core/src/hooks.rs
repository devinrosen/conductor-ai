use std::collections::HashMap;
use std::path::Path;

use crate::error::Result;
use crate::workflow::status::WorkflowRunStatus;

/// Per-workflow hook configuration parsed from `.conductor/config.toml`.
///
/// Example config:
/// ```toml
/// [hooks.ticket-to-pr]
/// on_fail = "debug-failed-run"
///
/// [hooks.review-pr]
/// on_complete = "analyze-patterns"
/// ```
#[derive(Debug, Clone, Default)]
pub struct HooksConfig {
    /// Map from workflow name → hook entry.
    pub hooks: HashMap<String, HookEntry>,
}

/// Hook triggers for a single workflow.
#[derive(Debug, Clone, Default)]
pub struct HookEntry {
    /// Workflow to trigger when the source workflow completes successfully.
    pub on_complete: Option<String>,
    /// Workflow to trigger when the source workflow fails.
    pub on_fail: Option<String>,
}

/// Load hook configuration from `.conductor/config.toml` in the given repo path.
///
/// Returns an empty config if the file does not exist or has no `[hooks]` section.
pub fn load_hooks_config(repo_path: &str) -> Result<HooksConfig> {
    let config_path = Path::new(repo_path).join(".conductor").join("config.toml");

    if !config_path.exists() {
        return Ok(HooksConfig::default());
    }

    let content = std::fs::read_to_string(&config_path).map_err(|e| {
        crate::error::ConductorError::Config(format!(
            "Failed to read {}: {e}",
            config_path.display()
        ))
    })?;

    parse_hooks_config(&content)
}

/// Parse hook configuration from TOML content.
fn parse_hooks_config(content: &str) -> Result<HooksConfig> {
    let table: toml::Table = content.parse().map_err(|e: toml::de::Error| {
        crate::error::ConductorError::Config(format!("Failed to parse config.toml: {e}"))
    })?;

    let Some(hooks_table) = table.get("hooks").and_then(|v| v.as_table()) else {
        return Ok(HooksConfig::default());
    };

    let mut hooks = HashMap::new();
    for (workflow_name, value) in hooks_table {
        let Some(entry_table) = value.as_table() else {
            continue;
        };
        let entry = HookEntry {
            on_complete: entry_table
                .get("on_complete")
                .and_then(|v| v.as_str())
                .map(String::from),
            on_fail: entry_table
                .get("on_fail")
                .and_then(|v| v.as_str())
                .map(String::from),
        };
        hooks.insert(workflow_name.clone(), entry);
    }

    Ok(HooksConfig { hooks })
}

/// Return workflow names that should be triggered as hooks for the given
/// workflow name and terminal status.
pub fn hooks_for(
    config: &HooksConfig,
    workflow_name: &str,
    status: &WorkflowRunStatus,
) -> Vec<String> {
    let Some(entry) = config.hooks.get(workflow_name) else {
        return Vec::new();
    };

    match status {
        WorkflowRunStatus::Completed => entry.on_complete.iter().cloned().collect(),
        WorkflowRunStatus::Failed => entry.on_fail.iter().cloned().collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_hooks_config() {
        let toml = r#"
[hooks.ticket-to-pr]
on_fail = "debug-failed-run"

[hooks.review-pr]
on_complete = "analyze-patterns"
on_fail = "debug-failed-run"
"#;
        let config = parse_hooks_config(toml).unwrap();
        assert_eq!(config.hooks.len(), 2);

        let ticket = config.hooks.get("ticket-to-pr").unwrap();
        assert_eq!(ticket.on_fail.as_deref(), Some("debug-failed-run"));
        assert!(ticket.on_complete.is_none());

        let review = config.hooks.get("review-pr").unwrap();
        assert_eq!(review.on_complete.as_deref(), Some("analyze-patterns"));
        assert_eq!(review.on_fail.as_deref(), Some("debug-failed-run"));
    }

    #[test]
    fn test_parse_no_hooks_section() {
        let toml = r#"
[some_other_section]
key = "value"
"#;
        let config = parse_hooks_config(toml).unwrap();
        assert!(config.hooks.is_empty());
    }

    #[test]
    fn test_parse_empty_content() {
        let config = parse_hooks_config("").unwrap();
        assert!(config.hooks.is_empty());
    }

    #[test]
    fn test_load_missing_file() {
        let config = load_hooks_config("/nonexistent/path").unwrap();
        assert!(config.hooks.is_empty());
    }

    #[test]
    fn test_hooks_for_completed() {
        let mut hooks = HashMap::new();
        hooks.insert(
            "my-wf".to_string(),
            HookEntry {
                on_complete: Some("post-complete".to_string()),
                on_fail: Some("post-fail".to_string()),
            },
        );
        let config = HooksConfig { hooks };

        let result = hooks_for(&config, "my-wf", &WorkflowRunStatus::Completed);
        assert_eq!(result, vec!["post-complete"]);
    }

    #[test]
    fn test_hooks_for_failed() {
        let mut hooks = HashMap::new();
        hooks.insert(
            "my-wf".to_string(),
            HookEntry {
                on_complete: Some("post-complete".to_string()),
                on_fail: Some("post-fail".to_string()),
            },
        );
        let config = HooksConfig { hooks };

        let result = hooks_for(&config, "my-wf", &WorkflowRunStatus::Failed);
        assert_eq!(result, vec!["post-fail"]);
    }

    #[test]
    fn test_hooks_for_unknown_workflow() {
        let config = HooksConfig::default();
        let result = hooks_for(&config, "unknown", &WorkflowRunStatus::Completed);
        assert!(result.is_empty());
    }

    #[test]
    fn test_hooks_for_non_terminal_status() {
        let mut hooks = HashMap::new();
        hooks.insert(
            "my-wf".to_string(),
            HookEntry {
                on_complete: Some("post-complete".to_string()),
                on_fail: Some("post-fail".to_string()),
            },
        );
        let config = HooksConfig { hooks };

        let result = hooks_for(&config, "my-wf", &WorkflowRunStatus::Running);
        assert!(result.is_empty());
    }
}
