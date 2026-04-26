use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::schema_config::{schema_to_tool_json, OutputSchema};
use crate::workflow::action_executor::{
    ActionExecutor, ActionOutput, ActionParams, ExecutionContext,
};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u64 = 8192;

const DEFAULT_API_MODEL: &str = "claude-sonnet-4-6";

struct ApiCallResult {
    json: serde_json::Value,
    json_string: String,
    input_tokens: i64,
    output_tokens: i64,
}

fn execute_via_api(
    prompt: &str,
    schema: &OutputSchema,
    model: &str,
    timeout: std::time::Duration,
    api_key: &str,
) -> std::result::Result<ApiCallResult, String> {
    let tool_json = schema_to_tool_json(schema);
    let body = serde_json::json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "tools": [tool_json],
        "tool_choice": {"type": "tool", "name": schema.name},
        "messages": [{"role": "user", "content": prompt}]
    });
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let response_result = agent
        .post(ANTHROPIC_API_URL)
        .set("x-api-key", api_key)
        .set("anthropic-version", ANTHROPIC_API_VERSION)
        .set("content-type", "application/json")
        .send_json(&body);
    let response_value: serde_json::Value = match response_result {
        Ok(resp) => resp
            .into_json()
            .map_err(|e| format!("Failed to parse API response JSON: {e}"))?,
        Err(ureq::Error::Status(status, resp)) => {
            let body_text = resp
                .into_string()
                .unwrap_or_else(|e| format!("<body read failed: {e}>"));
            let truncated = if body_text.len() > 500 {
                format!("{}…", &body_text[..500])
            } else {
                body_text
            };
            // The API may echo user-supplied prompt content in error bodies.
            // The 500-char cap above limits exposure; callers should treat this
            // error string as potentially containing user data.
            return Err(format!("API call failed: {status} {truncated}"));
        }
        Err(e) => return Err(format!("API call failed: {e}")),
    };
    let input = extract_tool_use_input(&response_value)?;
    let json_string = serde_json::to_string(&input)
        .map_err(|e| format!("Failed to serialize tool_use input: {e}"))?;
    let usage = response_value
        .get("usage")
        .unwrap_or(&serde_json::Value::Null);
    let input_tokens = usage
        .get("input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    Ok(ApiCallResult {
        json: input,
        json_string,
        input_tokens,
        output_tokens,
    })
}

fn extract_tool_use_input(
    response_value: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let content = response_value
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| "API response missing 'content' array".to_string())?;
    let tool_use_block = content
        .iter()
        .find(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .ok_or_else(|| "API response contained no tool_use block".to_string())?;
    tool_use_block
        .get("input")
        .ok_or_else(|| "tool_use block missing 'input' field".to_string())
        .cloned()
}

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

        let (_agent_def, prompt) = super::helpers::load_agent_and_build_prompt(ectx, params)?;

        let model = ectx.model.as_deref().unwrap_or(DEFAULT_API_MODEL);

        let result =
            execute_via_api(&prompt, schema, model, ectx.step_timeout, &api_key).map_err(|e| {
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
    fn test_extract_tool_use_input_success() {
        let response = serde_json::json!({
            "content": [
                {"type": "tool_use", "input": {"field": "value"}}
            ]
        });
        let result = extract_tool_use_input(&response).unwrap();
        assert_eq!(result["field"], "value");
    }

    #[test]
    fn test_missing_content_array() {
        let response = serde_json::json!({"model": "claude"});
        let err = extract_tool_use_input(&response).unwrap_err();
        assert!(err.contains("'content' array"), "got: {err}");
    }

    #[test]
    fn test_missing_tool_use_block() {
        let response = serde_json::json!({
            "content": [{"type": "text", "text": "hello"}]
        });
        let err = extract_tool_use_input(&response).unwrap_err();
        assert!(err.contains("no tool_use block"), "got: {err}");
    }

    #[test]
    fn test_tool_use_block_missing_input() {
        let response = serde_json::json!({
            "content": [{"type": "tool_use", "name": "my_tool"}]
        });
        let err = extract_tool_use_input(&response).unwrap_err();
        assert!(err.contains("missing 'input' field"), "got: {err}");
    }

    #[test]
    fn missing_schema_returns_error() {
        let executor = ApiCallExecutor::new(Config::default());
        let result = executor.execute(&make_ectx(), &make_action_params(None));
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("requires a schema"), "got: {msg}");
    }

    #[test]
    fn missing_api_key_returns_error() {
        // _guard (not _) keeps the mutex alive for the full test body to prevent env-var races.
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
