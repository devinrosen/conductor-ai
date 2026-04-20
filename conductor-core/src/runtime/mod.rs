//! AgentRuntime trait and dispatch infrastructure (RFC 007).
//!
//! # Extension points
//! - `AgentRuntime` — implement to add a new runtime (e.g. `CliRuntime`, `ScriptRuntime`).
//! - `resolve_runtime` — maps runtime name → boxed trait object; extend when adding runtimes.
//! - `RuntimeRequest` — carries per-invocation parameters from the workflow executor.

pub mod claude;

use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc};

use crate::agent::types::AgentRun;
use crate::agent_config::AgentDef;
use crate::config::{AgentPermissionMode, Config};
use crate::error::{ConductorError, Result};

/// Trait implemented by every agent runtime.
///
/// The lifecycle within the workflow executor is:
/// 1. `spawn(&request)` — launch the agent subprocess/API call.
/// 2. `poll(run_id, shutdown, step_timeout)` — block until the agent completes.
/// 3. On success `poll()` returns `Ok(AgentRun)` with the finalized run record.
///
/// `is_alive` and `cancel` are used by the orphan reaper and manual cancellation paths.
pub trait AgentRuntime {
    /// Launch the agent for `request`. Stores the handle internally.
    fn spawn(&self, request: &RuntimeRequest) -> Result<()>;

    /// Block until the agent completes or is cancelled.
    ///
    /// Opens its own DB connections internally — the caller does not need to pass one.
    fn poll(
        &self,
        run_id: &str,
        shutdown: Option<&Arc<AtomicBool>>,
        step_timeout: std::time::Duration,
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
    /// Claude permission mode (skip-permissions, auto-mode, plan, repo-safe).
    pub permission_mode: AgentPermissionMode,
    /// Optional model override.
    pub model: Option<String>,
    /// Custom Claude config directory (from `general.claude_config_dir`).
    pub config_dir: Option<String>,
    /// Bot identity name (from step or workflow default).
    pub bot_name: Option<String>,
    /// Extra plugin directories to search for agent definitions.
    pub plugin_dirs: Vec<String>,
}

/// Error returned by `AgentRuntime::poll`.
#[derive(Debug)]
pub enum PollError {
    /// Agent subprocess exited without emitting a `result` event.
    /// The caller should retry (within the retry budget) or mark the step failed.
    NoResult,
    /// Workflow shutdown was requested while the agent was running.
    /// The caller should update the step status and return immediately — do not retry.
    Cancelled,
    /// Poll failed with an explanatory message (e.g. DB unavailable, not yet spawned).
    /// The caller should treat this the same as `NoResult` for retry purposes.
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
///
/// Returns the built-in `ClaudeRuntime` for the name `"claude"`.
/// Returns `Err(ConductorError::Config(...))` for any other name in this release,
/// with a distinct message when the runtime is present in `config.runtimes`
/// (configured but not yet implemented) versus entirely unknown.
pub fn resolve_runtime(name: &str, config: &Config) -> Result<Box<dyn AgentRuntime>> {
    match name {
        "claude" => Ok(Box::new(claude::ClaudeRuntime::new())),
        other if config.runtimes.contains_key(other) => Err(ConductorError::Config(format!(
            "runtime '{other}' is defined in config but not yet implemented in this release; \
             only 'claude' is supported — CliRuntime and ScriptRuntime are planned for v0.7"
        ))),
        other => Err(ConductorError::Config(format!(
            "unknown runtime '{other}' — only 'claude' is supported in this release; \
             check the `runtime:` field in your agent frontmatter"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, RuntimeConfig};
    use std::collections::HashMap;

    fn config_with_runtime(name: &str) -> Config {
        let mut runtimes = HashMap::new();
        runtimes.insert(
            name.to_string(),
            RuntimeConfig {
                runtime_type: name.to_string(),
                binary: None,
                args: vec![],
                prompt_via: None,
                result_field: None,
                api_key_env: None,
                command: None,
                default_model: None,
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
    fn resolve_unknown_runtime_returns_err_with_unknown_message() {
        let config = Config::default();
        let err = resolve_runtime("gemini", &config)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("unknown runtime 'gemini'"), "got: {err}");
        assert!(err.contains("check the `runtime:` field"), "got: {err}");
    }

    #[test]
    fn resolve_configured_but_unimplemented_runtime_returns_distinct_err() {
        let config = config_with_runtime("gemini");
        let err = resolve_runtime("gemini", &config)
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("defined in config but not yet implemented"),
            "got: {err}"
        );
        assert!(err.contains("CliRuntime"), "got: {err}");
    }

    #[test]
    fn resolve_configured_runtime_does_not_match_unknown_message() {
        let config = config_with_runtime("codex");
        let err = resolve_runtime("codex", &config).err().unwrap().to_string();
        assert!(!err.contains("check the `runtime:` field"), "got: {err}");
    }
}
