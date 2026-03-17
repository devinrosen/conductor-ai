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
/// Finds the *first* occurrence — the real block delimiter always comes before any JSON content.
pub fn parse_conductor_output(text: &str) -> Option<ConductorOutput> {
    let start_marker = "<<<CONDUCTOR_OUTPUT>>>";
    let end_marker = "<<<END_CONDUCTOR_OUTPUT>>>";

    let start = text.find(start_marker)?;
    let json_start = start + start_marker.len();
    let end = text[json_start..].find(end_marker)?;
    let json_str = text[json_start..json_start + end].trim();

    serde_json::from_str(json_str).ok()
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

    /// Regression test: when the context field value contains the start marker string,
    /// `find` (not `rfind`) must be used so the real delimiter is found first.
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
}
