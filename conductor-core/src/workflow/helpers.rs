use super::action_executor::{ActionParams, ExecutionContext};

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
    let prompt = crate::workflow::prompt_builder::build_agent_prompt_from_params(&agent_def, params);
    Ok((agent_def, prompt))
}
