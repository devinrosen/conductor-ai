use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;

use crate::error::{ConductorError, Result};
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
#[derive(Debug, Default)]
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
/// Phase-1 version: conductor-specific fields will be generalized in Phase 2
/// when the engine is extracted to a standalone crate.
pub struct ExecutionContext {
    /// Pre-created `agent_runs` row ID for this invocation.
    pub run_id: String,
    /// Absolute path to the worktree root.
    pub working_dir: PathBuf,
    /// Absolute path to the repository root.
    pub repo_path: String,
    /// Absolute path to the SQLite database file.
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
}

/// Holds named and fallback `ActionExecutor` implementations.
///
/// Use `FlowEngineBuilder` to construct; call `dispatch` at step execution time.
pub struct ActionRegistry {
    named: HashMap<String, Box<dyn ActionExecutor>>,
    fallback: Option<Box<dyn ActionExecutor>>,
}

impl ActionRegistry {
    /// Construct a registry from pre-built maps (called only by `FlowEngineBuilder`).
    pub(super) fn new(
        named: HashMap<String, Box<dyn ActionExecutor>>,
        fallback: Option<Box<dyn ActionExecutor>>,
    ) -> Self {
        Self { named, fallback }
    }

    /// Find the executor for `name` and run it.
    ///
    /// Resolution order: exact-name match → fallback → error.
    pub fn dispatch(
        &self,
        name: &str,
        ectx: &ExecutionContext,
        params: &ActionParams,
    ) -> Result<ActionOutput> {
        let executor = self
            .named
            .get(name)
            .map(|e| e.as_ref())
            .or(self.fallback.as_deref());
        match executor {
            Some(e) => e.execute(ectx, params),
            None => Err(ConductorError::Workflow(format!(
                "no registered ActionExecutor for '{}' and no fallback configured",
                name
            ))),
        }
    }
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
            Ok(ActionOutput {
                markers: vec!["done".to_string()],
                context: Some("noop ran".to_string()),
                ..Default::default()
            })
        }
    }

    fn make_ectx() -> ExecutionContext {
        ExecutionContext {
            run_id: "run1".to_string(),
            working_dir: PathBuf::from("/tmp"),
            repo_path: "/tmp/repo".to_string(),
            db_path: PathBuf::from("/tmp/conductor.db"),
            step_timeout: Duration::from_secs(60),
            shutdown: None,
            model: None,
            bot_name: None,
            plugin_dirs: vec![],
            workflow_name: "test-wf".to_string(),
        }
    }

    fn make_params(name: &str) -> ActionParams {
        ActionParams {
            name: name.to_string(),
            inputs: HashMap::new(),
            retries_remaining: 0,
            retry_error: None,
            snippets: vec![],
            dry_run: false,
            gate_feedback: None,
            schema: None,
        }
    }

    #[test]
    fn dispatch_named_executor() {
        let registry = ActionRegistry::new(
            [(
                "noop".to_string(),
                Box::new(NoopExecutor) as Box<dyn ActionExecutor>,
            )]
            .into_iter()
            .collect(),
            None,
        );
        let ectx = make_ectx();
        let params = make_params("noop");
        let output = registry.dispatch("noop", &ectx, &params).unwrap();
        assert_eq!(output.markers, vec!["done"]);
    }

    #[test]
    fn dispatch_fallback_when_no_named_match() {
        let registry = ActionRegistry::new(
            std::collections::HashMap::new(),
            Some(Box::new(NoopExecutor)),
        );
        let ectx = make_ectx();
        let params = make_params("anything");
        let output = registry.dispatch("anything", &ectx, &params).unwrap();
        assert_eq!(output.markers, vec!["done"]);
    }

    #[test]
    fn dispatch_error_when_no_executor_or_fallback() {
        let registry = ActionRegistry::new(std::collections::HashMap::new(), None);
        let ectx = make_ectx();
        let params = make_params("missing");
        let err = registry.dispatch("missing", &ectx, &params).unwrap_err();
        assert!(
            err.to_string()
                .contains("no registered ActionExecutor for 'missing'"),
            "got: {err}"
        );
    }

    #[test]
    fn cancel_default_impl_is_noop() {
        let executor = NoopExecutor;
        assert!(executor.cancel("any-id").is_ok());
    }
}
