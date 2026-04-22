use std::collections::HashMap;

use crate::error::{ConductorError, Result};

use super::action_executor::{ActionExecutor, ActionRegistry};

/// Builder for constructing an `ActionRegistry`.
///
/// Call `.action()` to register named executors and `.action_fallback()` to
/// set the catch-all executor. Calling `.action_fallback()` more than once
/// causes the second call to return an error (enforced at call time, not at
/// `build()` time, so the builder chain remains infallible after the first call).
pub struct FlowEngineBuilder {
    named: HashMap<String, Box<dyn ActionExecutor>>,
    fallback: Option<Box<dyn ActionExecutor>>,
}

impl FlowEngineBuilder {
    pub fn new() -> Self {
        Self {
            named: HashMap::new(),
            fallback: None,
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
    pub fn action_fallback(mut self, executor: Box<dyn ActionExecutor>) -> Result<Self> {
        if self.fallback.is_some() {
            return Err(ConductorError::Workflow(
                "action_fallback already set — only one fallback executor is allowed".to_string(),
            ));
        }
        self.fallback = Some(executor);
        Ok(self)
    }

    /// Consume the builder and produce an `ActionRegistry`.
    pub fn build(self) -> Result<ActionRegistry> {
        Ok(ActionRegistry::new(self.named, self.fallback))
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
    use crate::workflow::action_executor::{ActionOutput, ActionParams, ExecutionContext};
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
        ) -> crate::error::Result<ActionOutput> {
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
        ) -> crate::error::Result<ActionOutput> {
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
            db_path: PathBuf::from("/tmp/db"),
            step_timeout: Duration::from_secs(60),
            shutdown: None,
            model: None,
            bot_name: None,
            plugin_dirs: vec![],
            workflow_name: "wf".to_string(),
        }
    }

    fn make_params(name: &str) -> ActionParams {
        ActionParams {
            name: name.to_string(),
            inputs: std::collections::HashMap::new(),
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
        let registry = FlowEngineBuilder::new()
            .action(Box::new(AlphaExecutor))
            .build()
            .unwrap();
        let output = registry
            .dispatch("alpha", &make_ectx(), &make_params("alpha"))
            .unwrap();
        assert_eq!(output.markers, vec!["alpha"]);
    }

    #[test]
    fn build_with_fallback() {
        let registry = FlowEngineBuilder::new()
            .action_fallback(Box::new(BetaExecutor))
            .unwrap()
            .build()
            .unwrap();
        let output = registry
            .dispatch("anything", &make_ectx(), &make_params("anything"))
            .unwrap();
        assert_eq!(output.markers, vec!["beta"]);
    }

    #[test]
    fn named_takes_precedence_over_fallback() {
        let registry = FlowEngineBuilder::new()
            .action(Box::new(AlphaExecutor))
            .action_fallback(Box::new(BetaExecutor))
            .unwrap()
            .build()
            .unwrap();
        let output = registry
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
