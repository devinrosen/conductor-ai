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
/// Returns `Err(ConductorError::Config(...))` for any other name in this release.
pub fn resolve_runtime(name: &str, _config: &Config) -> Result<Box<dyn AgentRuntime>> {
    match name {
        "claude" => Ok(Box::new(claude::ClaudeRuntime::new())),
        other => Err(ConductorError::Config(format!(
            "unknown runtime '{other}' — only 'claude' is supported in this release; \
             check the `runtime:` field in your agent frontmatter"
        ))),
    }
}
