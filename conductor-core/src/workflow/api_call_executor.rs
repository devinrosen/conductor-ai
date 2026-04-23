use crate::agent_config::AgentSpec;
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

        // Hot-reload: read agent definition fresh on every call (same pattern as ClaudeAgentExecutor).
        let working_dir_str = ectx.working_dir.to_string_lossy();
        let agent_def = crate::agent_config::load_agent(
            &working_dir_str,
            &ectx.repo_path,
            &AgentSpec::Name(params.name.clone()),
            Some(&ectx.workflow_name),
            &ectx.plugin_dirs,
        )?;

        let prompt =
            crate::workflow::prompt_builder::build_agent_prompt_from_params(&agent_def, params);

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
        .map_err(|e| ConductorError::Workflow(format!("API call for '{}' failed: {e}", params.name)))?;

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
    use crate::workflow::action_executor::{ActionParams, ExecutionContext};
    use std::collections::HashMap;
    use std::time::Duration;

    fn make_ectx() -> ExecutionContext {
        ExecutionContext {
            run_id: "run-1".to_string(),
            working_dir: std::path::PathBuf::from("/tmp"),
            repo_path: "/tmp".to_string(),
            db_path: std::path::PathBuf::from("/tmp/test.db"),
            step_timeout: Duration::from_secs(30),
            shutdown: None,
            model: None,
            bot_name: None,
            plugin_dirs: vec![],
            workflow_name: "test".to_string(),
            worktree_id: None,
            parent_run_id: "parent".to_string(),
            step_id: "step-1".to_string(),
        }
    }

    fn make_params(schema: Option<crate::schema_config::OutputSchema>) -> ActionParams {
        ActionParams {
            name: "test-agent".to_string(),
            inputs: HashMap::new(),
            retries_remaining: 0,
            retry_error: None,
            snippets: vec![],
            dry_run: false,
            gate_feedback: None,
            schema,
        }
    }

    #[test]
    fn missing_schema_returns_error() {
        let executor = ApiCallExecutor::new(Config::default());
        let result = executor.execute(&make_ectx(), &make_params(None));
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("requires a schema"), "got: {msg}");
    }

    #[test]
    fn missing_api_key_returns_error() {
        // Safety: single-threaded test manipulating env var; no other test
        // reads ANTHROPIC_API_KEY concurrently in the same process.
        let prev = std::env::var("ANTHROPIC_API_KEY").ok();
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };

        let schema = crate::schema_config::parse_schema_content(
            "fields:\n  ok: boolean\n",
            "test",
        )
        .unwrap();
        let executor = ApiCallExecutor::new(Config::default());
        let result = executor.execute(&make_ectx(), &make_params(Some(schema)));

        if let Some(key) = prev {
            unsafe { std::env::set_var("ANTHROPIC_API_KEY", key) };
        }

        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("ANTHROPIC_API_KEY"), "got: {msg}");
    }
}
