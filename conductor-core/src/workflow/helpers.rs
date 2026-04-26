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
    let prompt =
        crate::workflow::prompt_builder::build_agent_prompt_from_params(&agent_def, params);
    Ok((agent_def, prompt))
}
