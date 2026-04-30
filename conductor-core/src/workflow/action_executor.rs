use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;

use crate::error::Result;
use crate::schema_config::OutputSchema;

/// Trait for pluggable action execution.
///
/// Implementations must be `Send + Sync` to support parallel step execution
/// (`parallel.rs` spawns call steps across threads). The default `cancel`
/// implementation is a no-op; stateful executors should override it.
pub trait ActionExecutor: Send + Sync {
    #[allow(dead_code)]
    fn name(&self) -> &str;
    fn execute(&self, ectx: &ExecutionContext, params: &ActionParams) -> Result<ActionOutput>;
    #[allow(dead_code)]
    fn cancel(&self, execution_id: &str) -> Result<()> {
        let _ = execution_id;
        Ok(())
    }
}

/// Per-invocation inputs passed to an `ActionExecutor`.
#[derive(Clone)]
pub struct ActionParams {
    /// Short name of the agent/action being dispatched (e.g. `"plan"`).
    pub name: String,
    /// Fully-resolved template variable map — all `{{var}}` substitutions ready.
    /// Built from `prompt_builder::build_variable_map` at the dispatch site.
    pub inputs: HashMap<String, String>,
    /// Retries still available after this attempt (0 = last attempt).
    #[allow(dead_code)]
    pub retries_remaining: u32,
    /// Error message from the previous failed attempt, if any.
    pub retry_error: Option<String>,
    /// Pre-concatenated prompt snippet file contents (from `.conductor/prompts/`).
    /// Each element is the full text of one snippet file; the prompt builder joins them.
    pub snippets: Vec<String>,
    /// Whether this is a dry-run — commit-capable agents must not commit.
    pub dry_run: bool,
    /// Gate feedback string from the most recent gate step, if any.
    #[allow(dead_code)]
    pub gate_feedback: Option<String>,
    /// Output schema for structured output validation, if any.
    pub schema: Option<OutputSchema>,
}

/// Output produced by an `ActionExecutor` on success.
#[derive(Debug, Default, Clone)]
pub struct ActionOutput {
    pub markers: Vec<String>,
    pub context: Option<String>,
    pub result_text: Option<String>,
    pub structured_output: Option<String>,
    pub cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
}

/// Conductor-specific execution context passed to every `ActionExecutor::execute` call.
///
/// Carries per-invocation infrastructure (run ID, paths, timeouts) that the
/// executor needs to spawn and poll a subprocess or API call.
pub struct ExecutionContext {
    /// Pre-created `agent_runs` row ID for this invocation.
    pub run_id: String,
    /// Absolute path to the worktree root.
    pub working_dir: PathBuf,
    /// Absolute path to the repository root.
    pub repo_path: String,
    /// Absolute path to the SQLite database file (conductor-core specific).
    pub db_path: PathBuf,
    /// Per-step timeout (from `WorkflowExecConfig`).
    pub step_timeout: Duration,
    /// Shutdown signal shared with the workflow engine.
    pub shutdown: Option<Arc<AtomicBool>>,
    /// Resolved model override for this step (agent frontmatter model OR state model).
    pub model: Option<String>,
    /// Bot identity name for this step, if any.
    pub bot_name: Option<String>,
    /// Extra plugin directories to search for agent definitions.
    pub plugin_dirs: Vec<String>,
    /// Name of the parent workflow (used for workflow-local agent resolution).
    pub workflow_name: String,
    /// Worktree ID for this invocation, if any.
    #[allow(dead_code)]
    pub worktree_id: Option<String>,
    /// Parent workflow run ID.
    #[allow(dead_code)]
    pub parent_run_id: String,
    /// Workflow step ID for this invocation.
    #[allow(dead_code)]
    pub step_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopExecutor;

    impl ActionExecutor for NoopExecutor {
        fn name(&self) -> &str {
            "noop"
        }
        fn execute(
            &self,
            _ectx: &ExecutionContext,
            _params: &ActionParams,
        ) -> Result<ActionOutput> {
            Ok(ActionOutput::default())
        }
    }

    #[test]
    fn cancel_default_impl_is_noop() {
        let executor = NoopExecutor;
        assert!(executor.cancel("any-id").is_ok());
    }
}
