use crate::agent::AgentRunStatus;
use crate::agent_config::AgentSpec;
use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::runtime::PollError;
use crate::workflow::action_executor::{
    ActionExecutor, ActionOutput, ActionParams, ExecutionContext,
};

/// Wraps `AgentRuntime` dispatch behind the `ActionExecutor` trait.
///
/// Loads the agent `.md` definition at `execute()` time (not at registration
/// time) so that dropping a new file under `.conductor/agents/` takes effect
/// on the next workflow step without restarting the process (hot-reload).
pub struct ClaudeAgentExecutor {
    config: Config,
}

impl ClaudeAgentExecutor {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl ActionExecutor for ClaudeAgentExecutor {
    fn name(&self) -> &str {
        "__claude_agent__"
    }

    fn execute(&self, ectx: &ExecutionContext, params: &ActionParams) -> Result<ActionOutput> {
        // When a schema and API key are both present, delegate to ApiCallExecutor.
        // This preserves the architectural invariant: all call steps route through
        // ActionExecutor; ClaudeAgentExecutor is the subprocess fallback only.
        if params.schema.is_some() && self.config.anthropic_api_key().is_some() {
            return crate::workflow::api_call_executor::ApiCallExecutor::new(self.config.clone())
                .execute(ectx, params);
        }

        // Hot-reload: read the .md file fresh on every call so that new agent
        // definitions take effect without restarting the conductor process.
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

        let runtime = crate::runtime::resolve_runtime(&agent_def.runtime, &self.config)?;

        let request = crate::runtime::RuntimeRequest {
            run_id: ectx.run_id.clone(),
            agent_def,
            prompt,
            working_dir: ectx.working_dir.clone(),
            model: ectx.model.clone(),
            bot_name: ectx.bot_name.clone(),
            plugin_dirs: ectx.plugin_dirs.clone(),
            db_path: ectx.db_path.clone(),
        };

        runtime.spawn_validated(&request)?;

        let completed = match runtime.poll(
            &ectx.run_id,
            ectx.shutdown.as_ref(),
            ectx.step_timeout,
            &ectx.db_path,
        ) {
            Ok(run) => run,
            Err(PollError::Cancelled) => {
                return Err(ConductorError::WorkflowCancelled);
            }
            Err(e) => {
                return Err(ConductorError::Workflow(e.to_string()));
            }
        };

        let succeeded = completed.status == AgentRunStatus::Completed;

        let (markers, context, structured_output) =
            crate::workflow::output::interpret_agent_output(
                completed.result_text.as_deref(),
                params.schema.as_ref(),
                succeeded,
            )
            .map_err(ConductorError::Workflow)?;

        if succeeded {
            Ok(ActionOutput {
                markers,
                context: Some(context),
                result_text: completed.result_text,
                structured_output,
                cost_usd: completed.cost_usd,
                num_turns: completed.num_turns,
                duration_ms: completed.duration_ms,
                input_tokens: completed.input_tokens,
                output_tokens: completed.output_tokens,
                cache_read_input_tokens: completed.cache_read_input_tokens,
                cache_creation_input_tokens: completed.cache_creation_input_tokens,
            })
        } else {
            Err(ConductorError::Workflow(
                completed
                    .result_text
                    .unwrap_or_else(|| "unknown error".to_string()),
            ))
        }
    }
}
