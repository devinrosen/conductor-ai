use std::collections::HashMap;

use crate::error::{ConductorError, Result};

use super::action_executor::{ActionExecutor, ActionRegistry};

/// Builder for constructing an `ActionRegistry` used by the legacy conductor-core engine path.
///
/// Note: `runkon-flow` exports its own `FlowEngineBuilder` that constructs a full `FlowEngine`.
/// This type builds only an `ActionRegistry`; it is named `ActionRegistryBuilder` to avoid
/// confusion with the runkon-flow builder that exists in the same dependency tree.
pub struct ActionRegistryBuilder {
    named: HashMap<String, Box<dyn ActionExecutor>>,
    #[allow(dead_code)]
    fallback: Option<Box<dyn ActionExecutor>>,
}

impl ActionRegistryBuilder {
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub fn build(self) -> Result<ActionRegistry> {
        Ok(ActionRegistry::new(self.named, self.fallback))
    }
}

impl Default for ActionRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{make_ectx, make_params};
    use crate::workflow::action_executor::{ActionOutput, ActionParams, ExecutionContext};

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

    #[test]
    fn build_with_named_executor() {
        let registry = ActionRegistryBuilder::new()
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
        let registry = ActionRegistryBuilder::new()
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
        let registry = ActionRegistryBuilder::new()
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
        let result = ActionRegistryBuilder::new()
            .action_fallback(Box::new(AlphaExecutor))
            .unwrap()
            .action_fallback(Box::new(BetaExecutor));
        assert!(result.is_err(), "second action_fallback should return Err");
    }
}
