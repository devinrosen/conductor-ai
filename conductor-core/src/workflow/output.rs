use serde::{Deserialize, Serialize};

use crate::schema_config::OutputSchema;

/// Parsed output from `<<<CONDUCTOR_OUTPUT>>>` block.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConductorOutput {
    #[serde(default)]
    pub markers: Vec<String>,
    #[serde(default)]
    pub context: String,
}

/// Parse the `<<<CONDUCTOR_OUTPUT>>>` block from agent result text.
/// Finds the last occurrence immediately followed by `{`, `[`, or a code fence — the real block
/// delimiter. Strips markdown code fences before JSON parsing. This correctly skips occurrences
/// inside code examples, grep output, and JSON field values.
pub fn parse_conductor_output(text: &str) -> Option<ConductorOutput> {
    let cleaned = crate::schema_config::extract_output_block(text)?;
    serde_json::from_str(&cleaned).ok()
}

/// Interpret agent output using a schema (if present) or generic `CONDUCTOR_OUTPUT` parsing.
///
/// Returns `(markers, context, structured_json)`. The `succeeded` flag controls whether
/// a schema validation failure is treated as an error (`Err`) or silently falls back.
pub(super) fn interpret_agent_output(
    result_text: Option<&str>,
    schema: Option<&OutputSchema>,
    succeeded: bool,
) -> std::result::Result<(Vec<String>, String, Option<String>), String> {
    if let Some(s) = schema {
        match result_text.map(|text| crate::schema_config::parse_structured_output(text, s)) {
            Some(Ok(structured)) => Ok((
                structured.markers,
                structured.context,
                Some(structured.json_string),
            )),
            Some(Err(e)) if succeeded => {
                // Structured output validation failed on a successful run — caller should retry
                Err(format!("structured output validation: {e}"))
            }
            _ => {
                // No output block found or parsing error on a failed run — fall back
                let fallback = result_text
                    .and_then(parse_conductor_output)
                    .unwrap_or_default();
                Ok((fallback.markers, fallback.context, None))
            }
        }
    } else {
        let output = result_text
            .and_then(parse_conductor_output)
            .unwrap_or_default();
        Ok((output.markers, output.context, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Marker appears inside a JSON field value — must still find the real block.
    #[test]
    fn test_parse_conductor_output_marker_in_field_value() {
        let text = r#"Some agent output.
<<<CONDUCTOR_OUTPUT>>>
{
  "markers": ["done"],
  "context": "saw <<<CONDUCTOR_OUTPUT>>> in the log and handled it"
}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_conductor_output(text).unwrap();
        assert_eq!(result.markers, vec!["done"]);
        assert!(result.context.contains("<<<CONDUCTOR_OUTPUT>>>"));
    }

    /// Marker appears in code examples before the real block — must find the real block.
    #[test]
    fn test_parse_conductor_output_skips_code_examples() {
        let text = r#"Here is how to emit output:
```bash
echo '<<<CONDUCTOR_OUTPUT>>>'
echo '{"markers": ["fake"], "context": "example"}'
echo '<<<END_CONDUCTOR_OUTPUT>>>'
```

Actual output:
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["real"], "context": "this is the real result"}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_conductor_output(text).unwrap();
        assert_eq!(result.markers, vec!["real"]);
        assert_eq!(result.context, "this is the real result");
    }

    /// Multiple complete example blocks before the real one — must find the last real block.
    #[test]
    fn test_parse_conductor_output_multiple_complete_blocks() {
        let text = r#"Example 1:
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["example1"], "context": "first example"}
<<<END_CONDUCTOR_OUTPUT>>>

Example 2:
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["example2"], "context": "second example"}
<<<END_CONDUCTOR_OUTPUT>>>

Real output:
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["real"], "context": "the actual result"}
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_conductor_output(text).unwrap();
        assert_eq!(result.markers, vec!["real"]);
        assert_eq!(result.context, "the actual result");
    }

    /// Output block wrapped in a markdown code fence — must strip fences before parsing.
    #[test]
    fn test_parse_conductor_output_code_fenced() {
        let text = r#"Here is my output:
<<<CONDUCTOR_OUTPUT>>>
```json
{"markers": ["done"], "context": "fenced result"}
```
<<<END_CONDUCTOR_OUTPUT>>>
"#;
        let result = parse_conductor_output(text).unwrap();
        assert_eq!(result.markers, vec!["done"]);
        assert_eq!(result.context, "fenced result");
    }
}
