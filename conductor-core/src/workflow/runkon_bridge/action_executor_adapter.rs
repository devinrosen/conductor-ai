use std::sync::{Arc, Mutex};

use runkon_flow::engine_error::EngineError;

use super::bridge_lock_err;

/// Wraps conductor-core's `ClaudeAgentExecutor` behind the runkon-flow
/// `ActionExecutor` trait.
///
/// The runkon-flow `ExecutionContext` does not carry `db_path`, so we store it
/// in the adapter and inject it when constructing the portable `ClaudeAgentContext`.
pub(crate) struct RkActionExecutorAdapter {
    config: crate::config::Config,
    conn: Arc<Mutex<rusqlite::Connection>>,
    db_path: std::path::PathBuf,
}

impl RkActionExecutorAdapter {
    pub(crate) fn new(
        config: crate::config::Config,
        conn: Arc<Mutex<rusqlite::Connection>>,
        db_path: std::path::PathBuf,
    ) -> Self {
        Self {
            config,
            conn,
            db_path,
        }
    }
}

impl runkon_flow::traits::action_executor::ActionExecutor for RkActionExecutorAdapter {
    fn name(&self) -> &str {
        "__rk_claude_agent__"
    }

    fn execute(
        &self,
        ctx: &dyn runkon_flow::traits::run_context::RunContext,
        info: &runkon_flow::traits::action_executor::StepInfo,
        params: &runkon_flow::traits::action_executor::ActionParams,
    ) -> Result<runkon_flow::traits::action_executor::ActionOutput, EngineError> {
        // ClaudeAgentExecutor needs a pre-created agent_runs row ID as `run_id` so
        // it can track the subprocess. The step↔run link (child_run_id on the step
        // row) is written here — before execution starts — so the TUI can show live
        // agent output while the step is in flight. The engine also sets child_run_id
        // post-execution via ActionOutput, which is a no-op thanks to COALESCE.
        let child_run_id = {
            let conn = self.conn.lock().map_err(bridge_lock_err)?;
            let agent_mgr = crate::agent::AgentManager::new(&conn);
            let child_run = agent_mgr
                .create_child_run(
                    ctx.get(crate::workflow::engine_keys::WORKTREE_ID)
                        .as_deref(),
                    &format!("Workflow step: {}", params.name),
                    params.model.as_deref(),
                    ctx.parent_run_id().unwrap_or(""),
                    params.bot_name.as_deref(),
                )
                .map_err(|e| {
                    EngineError::Workflow(format!(
                        "step '{}': failed to create child agent run: {e}",
                        params.name
                    ))
                })?;

            if !info.step_id.is_empty() {
                // Best-effort pre-execution link so the TUI can show live agent output
                // while the step is running. The ActionOutput written by the executor
                // after execution completes is the authoritative source of child_run_id.
                if let Err(e) =
                    crate::workflow::update_step_child_run_id(&conn, &info.step_id, &child_run.id)
                {
                    tracing::warn!(
                        "step '{}' (step_id={}): failed to link child_run_id {}: {e}",
                        params.name,
                        info.step_id,
                        child_run.id,
                    );
                }
            }

            child_run.id
        };

        // Build per-step RuntimeOptions — max_turns is step-level so the resolver is fresh per call.
        let options = runkon_runtimes::RuntimeOptions {
            binary_path: crate::agent_runtime::resolve_conductor_bin().into(),
            log_path_for_run: std::sync::Arc::new(|run_id: &str| {
                crate::config::agent_log_path(run_id)
                    .unwrap_or_else(|_| std::env::temp_dir().join(format!("{run_id}.log")))
            }),
            workspace_root: self.config.general.workspace_root.clone(),
            argv_builder: crate::agent_runtime::conductor_argv_builder(),
            stall_threshold: Some(crate::agent_runtime::DEFAULT_STALL_THRESHOLD),
            max_turns: Some(
                params
                    .max_turns
                    .unwrap_or(crate::agent_runtime::DEFAULT_MAX_TURNS),
            ),
        };
        let resolver = std::sync::Arc::new(crate::runtime::adapter::ConductorRuntimeResolver {
            permission_mode: self
                .config
                .general
                .agent_permission_mode
                .to_runtime_permission_mode(),
            runtimes: self.config.runtimes.clone(),
            options,
        });

        let host_adapter = std::sync::Arc::new(
            crate::runtime::adapter::SqliteHostAdapter::new(self.db_path.clone())
                .map_err(|e| EngineError::Workflow(e.to_string()))?,
        );

        let agent_ctx = runkon_flow_executors::claude_agent::ClaudeAgentContext {
            run_id: child_run_id.clone(),
            working_dir: ctx.working_dir().to_path_buf(),
            repo_path: ctx
                .get(crate::workflow::engine_keys::REPO_PATH)
                .unwrap_or_default(),
            step_timeout: info.step_timeout,
            shutdown: ctx.shutdown().cloned(),
            model: params.model.clone(),
            bot_name: params.bot_name.clone(),
            plugin_dirs: params.plugin_dirs.clone(),
            workflow_name: ctx.workflow_name().to_string(),
            tracker: host_adapter.clone() as std::sync::Arc<dyn runkon_runtimes::RunTracker>,
            event_sink: host_adapter as std::sync::Arc<dyn runkon_runtimes::RunEventSink>,
        };
        let schema_arc = params
            .extensions
            .get::<crate::schema_config::OutputSchema>();
        let agent_params = runkon_flow_executors::claude_agent::ClaudeAgentParams {
            name: &params.name,
            inputs: &params.inputs,
            snippet_refs: &params.snippets,
            dry_run: params.dry_run,
            retry_error: params.retry_error.as_deref(),
            schema: schema_arc.as_deref(),
        };

        let inner = runkon_flow_executors::claude_agent::ClaudeAgentExecutor::new(
            resolver,
            self.config.anthropic_api_key(),
        );
        let mut output = inner
            .execute(&agent_ctx, &agent_params)
            .map_err(EngineError::Workflow)?;
        output.child_run_id = Some(child_run_id);
        Ok(output)
    }
}
