use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::workflow::action_executor::{
    ActionExecutor, ActionOutput, ActionParams, ExecutionContext,
};

/// Wraps `execute_via_api` behind the `ActionExecutor` trait for schema-constrained steps.
///
/// Routes to the Anthropic Messages API using `tool_use` enforcement, which makes
/// schema field mismatches impossible at the API level. Stateless: no subprocess
/// lifecycle, no pre-warmed pool. Hot-reloads the agent definition at execute time.
pub struct ApiCallExecutor {
    config: Config,
}

impl ApiCallExecutor {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl ActionExecutor for ApiCallExecutor {
    fn name(&self) -> &str {
        "__api_call__"
    }

    fn execute(&self, ectx: &ExecutionContext, params: &ActionParams) -> Result<ActionOutput> {
        let schema = params
            .schema
            .as_ref()
            .ok_or_else(|| ConductorError::Workflow("ApiCallExecutor requires a schema".into()))?;

        let api_key = self.config.anthropic_api_key().ok_or_else(|| {
            ConductorError::Workflow("ApiCallExecutor requires ANTHROPIC_API_KEY".into())
        })?;

        let (_agent_def, prompt) =
            super::helpers::load_agent_and_build_prompt(ectx, params)?;

        let model = ectx
            .model
            .as_deref()
            .unwrap_or(crate::workflow::executors::api_call::DEFAULT_API_MODEL);

        let result = crate::workflow::executors::api_call::execute_via_api(
            &prompt,
            schema,
            model,
            ectx.step_timeout,
            &api_key,
        )
        .map_err(|e| {
            ConductorError::Workflow(format!("API call for '{}' failed: {e}", params.name))
        })?;

        let structured = crate::schema_config::derive_output_from_value(result.json, schema);

        Ok(ActionOutput {
            result_text: Some(result.json_string),
            structured_output: Some(structured.json_string),
            markers: structured.markers,
            context: Some(structured.context),
            num_turns: Some(1),
            input_tokens: Some(result.input_tokens),
            output_tokens: Some(result.output_tokens),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::test_helpers::{make_action_params, make_ectx, ENV_MUTEX};

    #[test]
    fn missing_schema_returns_error() {
        let executor = ApiCallExecutor::new(Config::default());
        let result = executor.execute(&make_ectx(), &make_action_params(None));
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("requires a schema"), "got: {msg}");
    }

    #[test]
    fn missing_api_key_returns_error() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let prev = std::env::var("ANTHROPIC_API_KEY").ok();
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };

        let schema =
            crate::schema_config::parse_schema_content("fields:\n  ok: boolean\n", "test").unwrap();
        let executor = ApiCallExecutor::new(Config::default());
        let result = executor.execute(&make_ectx(), &make_action_params(Some(schema)));

        if let Some(key) = prev {
            unsafe { std::env::set_var("ANTHROPIC_API_KEY", key) };
        }

        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("ANTHROPIC_API_KEY"), "got: {msg}");
    }
}
