use super::action_executor::{ActionParams, ExecutionContext};

/// Parse a gate timeout string (e.g. `"1h"`, `"30m"`) into seconds.
///
/// Wraps `runkon_flow::dsl::parse_duration_str` so callers outside the bridge
/// layer do not import runkon-flow directly.
pub(crate) fn parse_gate_timeout_secs(s: &str) -> Option<i64> {
    match runkon_flow::dsl::parse_duration_str(s) {
        Ok(n) => i64::try_from(n).ok(),
        Err(_) => None,
    }
}

/// Format a list of gate selections into a human-readable context string.
pub fn format_gate_selection_context(items: &[String]) -> String {
    let mut out = String::from("User selected the following items:\n");
    for item in items {
        out.push_str("- ");
        out.push_str(item);
        out.push('\n');
    }
    out
}

/// Serialize an optional slice of gate selection strings to a JSON string.
///
/// Returns `Ok(None)` when `selections` is `None`, `Ok(Some(json))` on success,
/// or `Err(ConductorError::Workflow(...))` if serialization fails (should never
/// happen for `Vec<String>` but we propagate rather than panic).
pub(crate) fn serialize_gate_selections(
    selections: Option<&[String]>,
) -> crate::error::Result<Option<String>> {
    match selections {
        None => Ok(None),
        Some(s) => serde_json::to_string(s).map(Some).map_err(|e| {
            crate::error::ConductorError::Workflow(format!(
                "gate selections serialization failed: {e}"
            ))
        }),
    }
}

/// Parse a gate's stored `gate_options` JSON blob into a list of option strings.
///
/// The stored format is a JSON array of objects with a `"value"` key:
/// `[{"value": "opt1"}, {"value": "opt2"}]`
pub fn parse_gate_options(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<serde_json::Value>>(json)
        .ok()
        .map(|arr| {
            arr.into_iter()
                .filter_map(|v| v.get("value").and_then(|s| s.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn load_agent_and_build_prompt(
    ectx: &ExecutionContext,
    params: &ActionParams,
) -> crate::error::Result<(crate::agent_config::AgentDef, String)> {
    let working_dir_str = ectx.working_dir.to_string_lossy();
    let agent_def = crate::agent_config::load_agent(
        &working_dir_str,
        &ectx.repo_path,
        &crate::agent_config::AgentSpec::Name(params.name.clone()),
        Some(&ectx.workflow_name),
        &ectx.plugin_dirs,
    )?;

    // The runkon-flow engine passes snippet refs as names (from the DSL `with` field).
    // Resolve them to actual file contents before building the prompt.
    let mut resolved_params = params.clone();
    if !params.snippets.is_empty() {
        let snippet_text = crate::prompt_config::load_and_concat_snippets(
            &working_dir_str,
            &ectx.repo_path,
            &params.snippets,
            Some(&ectx.workflow_name),
        )?;
        resolved_params.snippets = if snippet_text.is_empty() {
            Vec::new()
        } else {
            vec![snippet_text]
        };
    }

    let prompt = crate::workflow::prompt_builder::build_agent_prompt_from_params(
        &agent_def,
        &resolved_params,
    );
    Ok((agent_def, prompt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gate_options_valid_array_returns_values() {
        let json = r#"[{"value": "opt1"}, {"value": "opt2"}, {"value": "opt3"}]"#;
        let result = parse_gate_options(json);
        assert_eq!(result, vec!["opt1", "opt2", "opt3"]);
    }

    #[test]
    fn parse_gate_options_invalid_json_returns_empty() {
        let result = parse_gate_options("not valid json at all");
        assert!(result.is_empty(), "invalid JSON should yield empty Vec");
    }

    #[test]
    fn parse_gate_options_drops_objects_missing_value_key() {
        let json = r#"[{"value": "keep"}, {"label": "no-value"}, {"value": "also-keep"}]"#;
        let result = parse_gate_options(json);
        assert_eq!(result, vec!["keep", "also-keep"]);
    }

    #[test]
    fn parse_gate_options_empty_array_returns_empty() {
        let result = parse_gate_options("[]");
        assert!(result.is_empty(), "empty array should yield empty Vec");
    }

    #[test]
    fn parse_gate_options_extra_fields_ignored() {
        let json = r#"[{"value": "a", "label": "A label", "extra": 42}]"#;
        let result = parse_gate_options(json);
        assert_eq!(result, vec!["a"]);
    }

    // ── format_gate_selection_context ────────────────────────────────────────

    #[test]
    fn format_gate_selection_context_empty_input() {
        let result = format_gate_selection_context(&[]);
        assert_eq!(result, "User selected the following items:\n");
    }

    #[test]
    fn format_gate_selection_context_single_item() {
        let result = format_gate_selection_context(&["alpha".to_string()]);
        assert_eq!(result, "User selected the following items:\n- alpha\n");
    }

    #[test]
    fn format_gate_selection_context_item_with_special_characters() {
        let result = format_gate_selection_context(&["say \"hello\"\nworld".to_string()]);
        assert_eq!(
            result,
            "User selected the following items:\n- say \"hello\"\nworld\n"
        );
    }

    #[test]
    fn format_gate_selection_context_multi_item_golden() {
        let items = vec!["foo".to_string(), "bar".to_string(), "baz qux".to_string()];
        let result = format_gate_selection_context(&items);
        assert_eq!(
            result,
            "User selected the following items:\n- foo\n- bar\n- baz qux\n"
        );
    }
}
