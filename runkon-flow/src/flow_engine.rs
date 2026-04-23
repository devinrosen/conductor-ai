use std::collections::HashMap;
use std::sync::Arc;

use crate::engine_error::EngineError;
use crate::traits::action_executor::{ActionExecutor, ActionRegistry};
use crate::traits::script_env_provider::{NoOpScriptEnvProvider, ScriptEnvProvider};

/// The output of `FlowEngineBuilder::build()`.
pub struct EngineBundle {
    pub action_registry: ActionRegistry,
    pub script_env_provider: Arc<dyn ScriptEnvProvider>,
}

/// Builder for constructing an `EngineBundle` (action registry + script env provider).
///
/// Call `.action()` to register named executors and `.action_fallback()` to
/// set the catch-all executor. Calling `.action_fallback()` more than once
/// causes the second call to return an error (enforced at call time, not at
/// `build()` time, so the builder chain remains infallible after the first call).
pub struct FlowEngineBuilder {
    named: HashMap<String, Box<dyn ActionExecutor>>,
    fallback: Option<Box<dyn ActionExecutor>>,
    script_env_provider: Box<dyn ScriptEnvProvider>,
}

impl FlowEngineBuilder {
    pub fn new() -> Self {
        Self {
            named: HashMap::new(),
            fallback: None,
            script_env_provider: Box::new(NoOpScriptEnvProvider),
        }
    }

    /// Register a named executor. The executor's `name()` is used as the lookup key.
    #[allow(dead_code)]
    pub fn action(mut self, executor: Box<dyn ActionExecutor>) -> Self {
        self.named.insert(executor.name().to_string(), executor);
        self
    }

    /// Register the fallback (catch-all) executor.
    ///
    /// Returns `Err` if called more than once — only one fallback is allowed.
    pub fn action_fallback(
        mut self,
        executor: Box<dyn ActionExecutor>,
    ) -> Result<Self, EngineError> {
        if self.fallback.is_some() {
            return Err(EngineError::Workflow(
                "action_fallback already set — only one fallback executor is allowed".to_string(),
            ));
        }
        self.fallback = Some(executor);
        Ok(self)
    }

    /// Set the script env provider. Defaults to `NoOpScriptEnvProvider`.
    pub fn script_env_provider(mut self, provider: Box<dyn ScriptEnvProvider>) -> Self {
        self.script_env_provider = provider;
        self
    }

    /// Consume the builder and produce an `EngineBundle`.
    pub fn build(self) -> Result<EngineBundle, EngineError> {
        Ok(EngineBundle {
            action_registry: ActionRegistry::new(self.named, self.fallback),
            script_env_provider: Arc::from(self.script_env_provider),
        })
    }
}

impl Default for FlowEngineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::action_executor::{ActionOutput, ActionParams, ExecutionContext};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    struct AlphaExecutor;
    impl ActionExecutor for AlphaExecutor {
        fn name(&self) -> &str {
            "alpha"
        }
        fn execute(
            &self,
            _ectx: &ExecutionContext,
            _params: &ActionParams,
        ) -> Result<ActionOutput, EngineError> {
            Ok(ActionOutput {
                markers: vec!["alpha".to_string()],
                ..Default::default()
            })
        }
    }

    struct BetaExecutor;
    impl ActionExecutor for BetaExecutor {
        fn name(&self) -> &str {
            "beta"
        }
        fn execute(
            &self,
            _ectx: &ExecutionContext,
            _params: &ActionParams,
        ) -> Result<ActionOutput, EngineError> {
            Ok(ActionOutput {
                markers: vec!["beta".to_string()],
                ..Default::default()
            })
        }
    }

    fn make_ectx() -> ExecutionContext {
        ExecutionContext {
            run_id: "r1".to_string(),
            working_dir: PathBuf::from("/tmp"),
            repo_path: "/tmp/repo".to_string(),
            step_timeout: Duration::from_secs(60),
            shutdown: None,
            model: None,
            bot_name: None,
            plugin_dirs: vec![],
            workflow_name: "wf".to_string(),
            worktree_id: None,
            parent_run_id: "parent-run-1".to_string(),
            step_id: "step-1".to_string(),
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
    fn build_with_named_executor() {
        let bundle = FlowEngineBuilder::new()
            .action(Box::new(AlphaExecutor))
            .build()
            .unwrap();
        let output = bundle
            .action_registry
            .dispatch("alpha", &make_ectx(), &make_params("alpha"))
            .unwrap();
        assert_eq!(output.markers, vec!["alpha"]);
    }

    #[test]
    fn build_with_fallback() {
        let bundle = FlowEngineBuilder::new()
            .action_fallback(Box::new(BetaExecutor))
            .unwrap()
            .build()
            .unwrap();
        let output = bundle
            .action_registry
            .dispatch("anything", &make_ectx(), &make_params("anything"))
            .unwrap();
        assert_eq!(output.markers, vec!["beta"]);
    }

    #[test]
    fn named_takes_precedence_over_fallback() {
        let bundle = FlowEngineBuilder::new()
            .action(Box::new(AlphaExecutor))
            .action_fallback(Box::new(BetaExecutor))
            .unwrap()
            .build()
            .unwrap();
        let output = bundle
            .action_registry
            .dispatch("alpha", &make_ectx(), &make_params("alpha"))
            .unwrap();
        assert_eq!(output.markers, vec!["alpha"]);
    }

    #[test]
    fn second_action_fallback_returns_err() {
        let result = FlowEngineBuilder::new()
            .action_fallback(Box::new(AlphaExecutor))
            .unwrap()
            .action_fallback(Box::new(BetaExecutor));
        assert!(result.is_err(), "second action_fallback should return Err");
    }
}
