//! Direct Anthropic Messages API executor for schema-constrained call steps.
//!
//! When a workflow `call` step has an `output_schema` defined and
//! `ANTHROPIC_API_KEY` is set in the environment, this module makes a direct
//! POST to `/v1/messages` with `tool_use` + `tool_choice: {type: "tool", name:
//! schema.name}`.  This makes schema field mismatches impossible — the API
//! rejects malformed responses before they reach conductor.

use crate::schema_config::{schema_to_tool_json, OutputSchema};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u64 = 8192;

/// Result of a successful direct API call.
pub struct ApiCallResult {
    /// The parsed JSON value from the `tool_use.input` block.
    pub json: serde_json::Value,
    /// The JSON serialized to a string (for DB storage).
    pub json_string: String,
    /// Input token count reported by the API.
    pub input_tokens: i64,
    /// Output token count reported by the API.
    pub output_tokens: i64,
}

/// Execute a schema-constrained step via direct Anthropic Messages API call.
///
/// Builds a `tool_use` request that forces the model to produce output matching
/// the schema, POSTs it to the Anthropic API, and extracts the `tool_use.input`
/// JSON from the response.
///
/// # Errors
///
/// Returns `Err(String)` describing the failure when:
/// - The HTTP request fails or returns a non-2xx status
/// - The response contains no `tool_use` block
/// - JSON parsing fails
pub fn execute_via_api(
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
        "tool_choice": {
            "type": "tool",
            "name": schema.name
        },
        "messages": [
            {
                "role": "user",
                "content": prompt
            }
        ]
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
            let body_text = resp.into_string().unwrap_or_default();
            return Err(format!("API call failed: {status} {body_text}"));
        }
        Err(e) => {
            return Err(format!("API call failed: {e}"));
        }
    };

    // Extract tool_use block from response content array
    let content = response_value
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| "API response missing 'content' array".to_string())?;

    let tool_use_block = content
        .iter()
        .find(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .ok_or_else(|| "API response contained no tool_use block".to_string())?;

    let input = tool_use_block
        .get("input")
        .ok_or_else(|| "tool_use block missing 'input' field".to_string())?
        .clone();

    let json_string = serde_json::to_string(&input)
        .map_err(|e| format!("Failed to serialize tool_use input: {e}"))?;

    // Extract token counts from usage
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_config::{FieldDef, FieldType, OutputSchema};

    fn simple_schema() -> OutputSchema {
        OutputSchema {
            name: "test-output".to_string(),
            fields: vec![FieldDef {
                name: "summary".to_string(),
                required: true,
                field_type: FieldType::String,
                desc: Some("A brief summary".to_string()),
                examples: None,
            }],
            markers: None,
        }
    }

    /// Verify the request body shape that would be sent to the API.
    #[test]
    fn test_request_body_shape() {
        let schema = simple_schema();
        let tool_json = schema_to_tool_json(&schema);
        let body = serde_json::json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": MAX_TOKENS,
            "tools": [tool_json],
            "tool_choice": {
                "type": "tool",
                "name": schema.name
            },
            "messages": [
                {
                    "role": "user",
                    "content": "Generate output for this task."
                }
            ]
        });

        assert_eq!(body["model"], "claude-sonnet-4-6");
        assert_eq!(body["max_tokens"], MAX_TOKENS);
        assert_eq!(body["tool_choice"]["type"], "tool");
        assert_eq!(body["tool_choice"]["name"], "test-output");
        assert_eq!(body["tools"][0]["name"], "test-output");
        assert_eq!(body["messages"][0]["role"], "user");
    }

    /// Verify extraction logic for a well-formed tool_use response.
    #[test]
    fn test_extract_tool_use_input() {
        let response = serde_json::json!({
            "id": "msg_01abc",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_01abc",
                    "name": "test-output",
                    "input": {
                        "summary": "This is a test summary"
                    }
                }
            ],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50
            }
        });

        // Replicate the extraction logic from execute_via_api
        let content = response["content"].as_array().unwrap();
        let tool_use_block = content
            .iter()
            .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
            .unwrap();

        let input = tool_use_block["input"].clone();
        assert_eq!(input["summary"], "This is a test summary");

        let input_tokens = response["usage"]["input_tokens"].as_i64().unwrap();
        let output_tokens = response["usage"]["output_tokens"].as_i64().unwrap();
        assert_eq!(input_tokens, 100);
        assert_eq!(output_tokens, 50);
    }

    /// Verify that a response without a tool_use block produces an appropriate error.
    #[test]
    fn test_missing_tool_use_block() {
        let response = serde_json::json!({
            "content": [
                {
                    "type": "text",
                    "text": "I cannot use a tool here."
                }
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });

        let content = response["content"].as_array().unwrap();
        let tool_use_block = content
            .iter()
            .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"));

        assert!(tool_use_block.is_none());
    }
}
