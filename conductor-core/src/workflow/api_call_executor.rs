use crate::error::{ConductorError, Result};
use crate::schema_config::OutputSchema;
use crate::workflow::action_executor::{
    ActionExecutor, ActionOutput, ActionParams, ExecutionContext,
};

/// Wraps `runkon_flow_executors::anthropic_api::ApiCallExecutor` behind the
/// `ActionExecutor` trait for schema-constrained steps.
///
/// Stateless: no subprocess lifecycle, no pre-warmed pool. Hot-reloads the
/// agent definition at execute time.
pub struct ApiCallExecutor {
    api_key: String,
}

impl ApiCallExecutor {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

impl ActionExecutor for ApiCallExecutor {
    fn name(&self) -> &str {
        "__api_call__"
    }

    fn execute(&self, ectx: &ExecutionContext, params: &ActionParams) -> Result<ActionOutput> {
        let schema = params
            .extensions
            .get::<OutputSchema>()
            .ok_or_else(|| ConductorError::Workflow("ApiCallExecutor requires a schema".into()))?;

        if self.api_key.is_empty() {
            return Err(ConductorError::Workflow(
                "ApiCallExecutor requires ANTHROPIC_API_KEY".into(),
            ));
        }

        let (_agent_def, prompt) = super::helpers::load_agent_and_build_prompt(ectx, params)?;

        let model = ectx
            .model
            .as_deref()
            .unwrap_or(runkon_flow_executors::anthropic_api::DEFAULT_API_MODEL);

        let rk_executor =
            runkon_flow_executors::anthropic_api::ApiCallExecutor::new(self.api_key.clone());

        let output = rk_executor
            .execute(&prompt, schema.as_ref(), model, ectx.step_timeout)
            .map_err(|e| {
                ConductorError::Workflow(format!("API call for '{}' failed: {e}", params.name))
            })?;

        Ok(ActionOutput {
            result_text: Some(output.result_text),
            structured_output: Some(output.structured_output),
            markers: output.markers,
            context: Some(output.context),
            metadata: output.metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{make_action_params, make_ectx};

    #[test]
    fn missing_schema_returns_error() {
        let executor = ApiCallExecutor::new("dummy-key".to_string());
        let result = executor.execute(&make_ectx(), &make_action_params(None));
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("requires a schema"), "got: {msg}");
    }

    #[test]
    fn missing_api_key_returns_error() {
        let schema =
            crate::schema_config::parse_schema_content("fields:\n  ok: boolean\n", "test").unwrap();
        let executor = ApiCallExecutor::new("".to_string());
        let result = executor.execute(&make_ectx(), &make_action_params(Some(schema)));
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("ANTHROPIC_API_KEY"), "got: {msg}");
    }
}
