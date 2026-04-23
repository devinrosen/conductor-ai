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
///
/// When `api_executor` is supplied and both a schema and an API key are present,
/// the call is forwarded to `api_executor` rather than spawning a subprocess.
pub struct ClaudeAgentExecutor {
    config: Config,
    api_executor: Option<Box<dyn ActionExecutor>>,
}

impl ClaudeAgentExecutor {
    pub fn new(config: Config, api_executor: Option<Box<dyn ActionExecutor>>) -> Self {
        Self {
            config,
            api_executor,
        }
    }
}

impl ActionExecutor for ClaudeAgentExecutor {
    fn name(&self) -> &str {
        "__claude_agent__"
    }

    fn execute(&self, ectx: &ExecutionContext, params: &ActionParams) -> Result<ActionOutput> {
        // When a schema and API key are both present, delegate to the injected
        // api_executor.  Routing through a trait reference preserves the
        // ActionExecutor abstraction — no concrete peer dependency.
        if let Some(ref api_exec) = self.api_executor {
            if params.schema.is_some() && self.config.anthropic_api_key().is_some() {
                return api_exec.execute(ectx, params);
            }
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
            let detail = completed.result_text.unwrap_or_else(|| {
                format!("agent '{}' completed with status {:?} but no result text", params.name, completed.status)
            });
            Err(ConductorError::Workflow(detail))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::error::Result;
    use crate::workflow::action_executor::{ActionOutput, ActionParams, ExecutionContext};
    use std::collections::HashMap;
    use std::time::Duration;

    struct MockApiExecutor {
        called: std::sync::atomic::AtomicBool,
    }

    impl MockApiExecutor {
        fn new() -> Self {
            Self {
                called: std::sync::atomic::AtomicBool::new(false),
            }
        }

        fn was_called(&self) -> bool {
            self.called.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl ActionExecutor for MockApiExecutor {
        fn name(&self) -> &str {
            "__mock__"
        }

        fn execute(&self, _ectx: &ExecutionContext, _params: &ActionParams) -> Result<ActionOutput> {
            self.called
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(ActionOutput {
                markers: vec!["mock_marker".to_string()],
                context: Some("mock context".to_string()),
                ..Default::default()
            })
        }
    }

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

    fn make_schema() -> crate::schema_config::OutputSchema {
        crate::schema_config::parse_schema_content("fields:\n  ok: boolean\n", "test").unwrap()
    }

    #[test]
    fn delegates_to_api_executor_when_schema_and_key_present() {
        let prev = std::env::var("ANTHROPIC_API_KEY").ok();
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "test-key") };

        let mock = Box::new(MockApiExecutor::new());
        // SAFETY: we read `was_called` after `execute`, which runs synchronously.
        let mock_ptr = mock.as_ref() as *const MockApiExecutor;
        let executor = ClaudeAgentExecutor::new(Config::default(), Some(mock));

        let result = executor.execute(&make_ectx(), &make_params(Some(make_schema())));

        // Restore env before any assertion that might panic.
        match prev {
            Some(k) => unsafe { std::env::set_var("ANTHROPIC_API_KEY", k) },
            None => unsafe { std::env::remove_var("ANTHROPIC_API_KEY") },
        }

        assert!(result.is_ok(), "expected Ok, got {:?}", result.unwrap_err());
        // SAFETY: mock is still alive — it was moved into executor which is still in scope.
        assert!(unsafe { &*mock_ptr }.was_called(), "api_executor was not called");
    }

    #[test]
    fn skips_api_executor_when_schema_absent() {
        let prev = std::env::var("ANTHROPIC_API_KEY").ok();
        unsafe { std::env::set_var("ANTHROPIC_API_KEY", "test-key") };

        let mock = Box::new(MockApiExecutor::new());
        let mock_ptr = mock.as_ref() as *const MockApiExecutor;
        let executor = ClaudeAgentExecutor::new(Config::default(), Some(mock));

        // No schema → should NOT delegate; falls through to load_agent, which fails
        // because /tmp has no .conductor/agents directory.
        let result = executor.execute(&make_ectx(), &make_params(None));

        match prev {
            Some(k) => unsafe { std::env::set_var("ANTHROPIC_API_KEY", k) },
            None => unsafe { std::env::remove_var("ANTHROPIC_API_KEY") },
        }

        // SAFETY: mock is still alive — moved into executor.
        assert!(!unsafe { &*mock_ptr }.was_called(), "api_executor must not be called without a schema");
        // load_agent fails on /tmp — that's expected, confirms no delegation happened.
        assert!(result.is_err());
    }

    #[test]
    fn skips_api_executor_when_api_key_absent() {
        let prev = std::env::var("ANTHROPIC_API_KEY").ok();
        unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };

        let mock = Box::new(MockApiExecutor::new());
        let mock_ptr = mock.as_ref() as *const MockApiExecutor;
        let executor = ClaudeAgentExecutor::new(Config::default(), Some(mock));

        let result = executor.execute(&make_ectx(), &make_params(Some(make_schema())));

        if let Some(k) = prev {
            unsafe { std::env::set_var("ANTHROPIC_API_KEY", k) };
        }

        // SAFETY: mock is still alive.
        assert!(!unsafe { &*mock_ptr }.was_called(), "api_executor must not be called without API key");
        assert!(result.is_err());
    }
}
