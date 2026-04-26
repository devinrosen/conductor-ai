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
