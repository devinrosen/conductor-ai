//! AgentRuntime trait and dispatch infrastructure (RFC 007).
//!
//! # Extension points
//! - `AgentRuntime` — implement to add a new runtime (e.g. `CliRuntime`, `ScriptRuntime`).
//! - `resolve_runtime` — maps runtime name → boxed trait object; extend when adding runtimes.
//! - `RuntimeRequest` — carries per-invocation parameters from the workflow executor.

pub mod claude;
pub mod cli;
pub mod script;

use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc};

use crate::agent::types::AgentRun;
use crate::agent_config::AgentDef;
use crate::config::Config;
use crate::error::{ConductorError, Result};

/// Trait implemented by every agent runtime.
pub trait AgentRuntime {
    /// Launch the agent for `request`. Stores the handle internally.
    /// Implementors must NOT call `validate_run_id` — that is handled by `spawn_validated`.
    fn spawn_impl(&self, request: &RuntimeRequest) -> Result<()>;

    /// Validates `request.run_id` then delegates to `spawn_impl`.
    /// This is the method callers should use.
    fn spawn_validated(&self, request: &RuntimeRequest) -> Result<()> {
        crate::text_util::validate_run_id(&request.run_id)?;
        self.spawn_impl(request)
    }

    /// Block until the agent completes or is cancelled.
    fn poll(
        &self,
        run_id: &str,
        shutdown: Option<&Arc<AtomicBool>>,
        step_timeout: std::time::Duration,
        db_path: &std::path::Path,
    ) -> std::result::Result<AgentRun, PollError>;

    /// Returns true if the agent process / session represented by `run` is still live.
    fn is_alive(&self, run: &AgentRun) -> bool;

    /// Forcibly cancel the agent represented by `run`.
    fn cancel(&self, run: &AgentRun) -> Result<()>;
}

/// Per-invocation parameters passed to `AgentRuntime::spawn`.
pub struct RuntimeRequest {
    /// The agent_runs row ID for this invocation.
    pub run_id: String,
    /// Resolved agent definition (from `.md` file).
    pub agent_def: AgentDef,
    /// Fully-rendered prompt string.
    pub prompt: String,
    /// Absolute path to the worktree root.
    pub working_dir: PathBuf,
    /// Optional model override.
    pub model: Option<String>,
    /// Bot identity name (from step or workflow default).
    pub bot_name: Option<String>,
    /// Extra plugin directories to search for agent definitions.
    pub plugin_dirs: Vec<String>,
    /// Absolute path to the SQLite database file.
    pub db_path: PathBuf,
}

/// Error returned by `AgentRuntime::poll`.
#[derive(Debug)]
pub enum PollError {
    /// Agent subprocess exited without emitting a `result` event.
    NoResult,
    /// Workflow shutdown was requested while the agent was running.
    Cancelled,
    /// Poll failed with an explanatory message.
    Failed(String),
}

impl std::fmt::Display for PollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoResult => write!(f, "agent exited without result"),
            Self::Cancelled => write!(f, "executor shutdown requested"),
            Self::Failed(msg) => write!(f, "{msg}"),
        }
    }
}

/// Resolve a runtime name to a boxed `AgentRuntime` implementation.
pub fn resolve_runtime(name: &str, config: &Config) -> Result<Box<dyn AgentRuntime>> {
    if name == "claude" {
        let options = claude::ClaudeRuntimeOptions {
            permission_mode: config.general.agent_permission_mode,
            config_dir: config.general.claude_config_dir.clone(),
        };
        return Ok(Box::new(claude::ClaudeRuntime::new(options)));
    }
    let rt_config = config.runtimes.get(name).ok_or_else(|| {
        ConductorError::Config(format!(
            "unknown runtime '{name}' — only 'claude' is built-in; \
                 add a `[runtimes.{name}]` section to conductor.toml for CLI agents"
        ))
    })?;
    match rt_config.runtime_type.as_deref().unwrap_or("cli") {
        "cli" => Ok(Box::new(cli::CliRuntime::new(rt_config.clone()))),
        "script" => Ok(Box::new(script::ScriptRuntime::new(rt_config.clone()))),
        t => Err(ConductorError::Config(format!(
            "unsupported runtime type '{t}' for '{name}'"
        ))),
    }
}

/// Extract a value from a serde_json::Value using a dot-separated path.
///
/// A `*` segment gathers all values at the current level and sums them if
/// they are numbers (used for token_fields aggregation).
pub fn extract_json_path(value: &serde_json::Value, path: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = path.split('.').collect();
    extract_path_recursive(value, &parts)
}

fn extract_path_recursive(value: &serde_json::Value, parts: &[&str]) -> Option<serde_json::Value> {
    if parts.is_empty() {
        return Some(value.clone());
    }
    let head = parts[0];
    let tail = &parts[1..];
    if head == "*" {
        let children: Vec<serde_json::Value> = match value {
            serde_json::Value::Object(m) => m.values().cloned().collect(),
            serde_json::Value::Array(a) => a.clone(),
            _ => return None,
        };
        if tail.is_empty() {
            return Some(serde_json::Value::Array(children));
        }
        let sum: f64 = children
            .iter()
            .filter_map(|child| extract_path_recursive(child, tail))
            .filter_map(|v| v.as_f64())
            .sum();
        return Some(serde_json::json!(sum));
    }
    match value {
        serde_json::Value::Object(m) => {
            let child = m.get(head)?;
            extract_path_recursive(child, tail)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, RuntimeConfig};
    use serde_json::json;
    use std::collections::HashMap;

    fn config_with_runtime(name: &str, rt_type: &str) -> Config {
        let mut runtimes = HashMap::new();
        runtimes.insert(
            name.to_string(),
            RuntimeConfig {
                runtime_type: Some(rt_type.to_string()),
                ..RuntimeConfig::default()
            },
        );
        Config {
            runtimes,
            ..Config::default()
        }
    }

    #[test]
    fn resolve_claude_returns_ok() {
        let config = Config::default();
        assert!(resolve_runtime("claude", &config).is_ok());
    }

    #[test]
    fn resolve_unknown_runtime_returns_err() {
        let config = Config::default();
        let err = resolve_runtime("gemini", &config)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("unknown runtime 'gemini'"), "got: {err}");
    }

    #[test]
    fn resolve_configured_cli_runtime_returns_ok() {
        let config = config_with_runtime("gemini", "cli");
        assert!(resolve_runtime("gemini", &config).is_ok());
    }

    #[test]
    fn resolve_unsupported_runtime_type_returns_err() {
        let config = config_with_runtime("myapi", "api");
        let err = resolve_runtime("myapi", &config).err().unwrap().to_string();
        assert!(err.contains("unsupported runtime type"), "got: {err}");
    }

    #[test]
    fn test_extract_simple_field() {
        let v = json!({"response": "hello", "status": "ok"});
        assert_eq!(extract_json_path(&v, "response"), Some(json!("hello")));
    }

    #[test]
    fn test_extract_nested_field() {
        let v = json!({"stats": {"total": 42}});
        assert_eq!(extract_json_path(&v, "stats.total"), Some(json!(42)));
    }

    #[test]
    fn test_extract_wildcard_sum() {
        let v = json!({
            "models": {
                "a": {"tokens": {"total": 100}},
                "b": {"tokens": {"total": 200}}
            }
        });
        let result = extract_json_path(&v, "models.*.tokens.total");
        assert!(result.is_some());
        let n = result.unwrap().as_f64().unwrap();
        assert!((n - 300.0).abs() < 0.01);
    }

    #[test]
    fn test_extract_missing_returns_none() {
        let v = json!({"a": 1});
        assert!(extract_json_path(&v, "b").is_none());
    }
}
