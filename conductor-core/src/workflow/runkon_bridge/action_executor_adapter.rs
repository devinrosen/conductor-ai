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
    /// Workflow-run-level runtime override. When `Some`, this is the runtime
    /// the host (e.g. the model picker) explicitly chose; the executor uses
    /// it instead of `agent_def.runtime`. When `None`, the adapter falls
    /// back to deriving a runtime from `params.model`.
    runtime_override: Option<String>,
}

impl RkActionExecutorAdapter {
    pub(crate) fn new(
        config: crate::config::Config,
        conn: Arc<Mutex<rusqlite::Connection>>,
        db_path: std::path::PathBuf,
        runtime_override: Option<String>,
    ) -> Self {
        Self {
            config,
            conn,
            db_path,
            runtime_override,
        }
    }
}

/// Walk `runtimes` for the first non-built-in entry whose `supported_models`
/// contains `model`. Used to route a model name to the runtime that owns it
/// when the host did not pass an explicit runtime selection (e.g. CLI workflow
/// runs that only specify `--model`). Returns `None` when no match is found
/// or `model` is `None`.
///
/// Iterates in `BTreeMap`-style sorted name order for determinism so that two
/// runtimes claiming the same model resolve consistently across invocations.
pub(super) fn derive_runtime_from_model(
    model: Option<&str>,
    runtimes: &std::collections::HashMap<String, runkon_runtimes::config::RuntimeConfig>,
) -> Option<String> {
    let m = model?;
    let mut names: Vec<&String> = runtimes.keys().filter(|n| n.as_str() != "claude").collect();
    names.sort();
    for name in names {
        if let Some(rt) = runtimes.get(name) {
            if rt.supported_models.iter().any(|s| s == m) {
                return Some(name.clone());
            }
        }
    }
    None
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
                    params.as_identity.as_deref(),
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
            env: Default::default(),
            log_path_for_run: std::sync::Arc::new(|run_id: &str| {
                crate::config::agent_log_path(run_id)
                    .unwrap_or_else(|_| std::env::temp_dir().join(format!("{run_id}.log")))
            }),
            workspace_root: self.config.general.workspace_root.clone(),
            stall_threshold: Some(self.config.agents.stall_threshold()),
            max_turns: self.config.agents.workflow_max_turns(
                params
                    .extensions
                    .get::<runkon_flow::extensions::ClaudeActionParams>()
                    .and_then(|p| p.max_turns),
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

        // Resolve the runtime the executor should use:
        //   1. The workflow-run-level explicit override (set by the host when
        //      e.g. the model picker chose a non-default runtime).
        //   2. Derived from the requested model — first non-built-in runtime
        //      whose `supported_models` contains it.
        //   3. `None` → executor falls back to `agent_def.runtime` from the
        //      agent file's frontmatter (or the host's `default_runtime`).
        let runtime_override = self
            .runtime_override
            .clone()
            .or_else(|| derive_runtime_from_model(params.model.as_deref(), &self.config.runtimes));

        // `create_child_run` hardcodes `runtime = "claude"` at row creation
        // (it has no way to know the eventual resolution). Patch the row now
        // that we've resolved it, so the DB reflects the runtime that will
        // actually launch the subprocess.
        if let Some(rt) = runtime_override.as_deref() {
            let conn = self.conn.lock().map_err(bridge_lock_err)?;
            let agent_mgr = crate::agent::AgentManager::new(&conn);
            if let Err(e) = agent_mgr.update_run_runtime(&child_run_id, rt) {
                tracing::warn!(
                    "step '{}': failed to update agent_runs.runtime to '{}': {e}",
                    params.name,
                    rt,
                );
            }
        }

        let agent_ctx = runkon_anthropic::claude_agent::ClaudeAgentContext {
            run_id: child_run_id.clone(),
            working_dir: ctx.working_dir().to_path_buf(),
            repo_path: ctx
                .get(crate::workflow::engine_keys::REPO_PATH)
                .unwrap_or_default(),
            step_timeout: info.step_timeout,
            shutdown: ctx.shutdown().cloned(),
            model: params.model.clone(),
            bot_name: params.as_identity.clone(),
            plugin_dirs: params.plugin_dirs.clone(),
            workflow_name: ctx.workflow_name().to_string(),
            tracker: host_adapter.clone() as std::sync::Arc<dyn runkon_runtimes::RunTracker>,
            event_sink: host_adapter as std::sync::Arc<dyn runkon_runtimes::RunEventSink>,
            runtimes: self.config.runtimes.clone(),
            default_runtime: self.config.general.default_runtime.clone(),
            runtime_override,
        };
        let schema_arc = params
            .extensions
            .get::<crate::schema_config::OutputSchema>();
        let agent_params = runkon_anthropic::claude_agent::ClaudeAgentParams {
            name: &params.name,
            inputs: &params.inputs,
            snippet_refs: &params.snippets,
            dry_run: params.dry_run,
            retry_error: params.retry_error.as_deref(),
            schema: schema_arc.as_deref(),
        };

        let inner = runkon_anthropic::claude_agent::ClaudeAgentExecutor::new(
            resolver,
            None, // Always use runtime resolver; API key path bypasses runtime selection
        );
        let mut output = inner
            .execute(&agent_ctx, &agent_params)
            .map_err(EngineError::Workflow)?;
        output.child_run_id = Some(child_run_id);
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::derive_runtime_from_model;
    use runkon_runtimes::config::RuntimeConfig;
    use std::collections::HashMap;

    fn rt(models: &[&str]) -> RuntimeConfig {
        RuntimeConfig {
            runtime_type: Some("claude".into()),
            supported_models: models.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn returns_none_when_model_is_none() {
        let mut runtimes = HashMap::new();
        runtimes.insert("qwen-local".to_string(), rt(&["qwen-72b"]));
        assert_eq!(derive_runtime_from_model(None, &runtimes), None);
    }

    #[test]
    fn returns_none_when_no_runtime_supports_model() {
        let mut runtimes = HashMap::new();
        runtimes.insert("qwen-local".to_string(), rt(&["qwen-72b"]));
        assert_eq!(
            derive_runtime_from_model(Some("not-anywhere"), &runtimes),
            None
        );
    }

    #[test]
    fn returns_runtime_owning_the_model() {
        let mut runtimes = HashMap::new();
        runtimes.insert(
            "qwen-local".to_string(),
            rt(&["QuantTrio/Qwen3.5-122B-A10B-AWQ"]),
        );
        runtimes.insert("other".to_string(), rt(&["whatever"]));
        assert_eq!(
            derive_runtime_from_model(Some("QuantTrio/Qwen3.5-122B-A10B-AWQ"), &runtimes),
            Some("qwen-local".to_string())
        );
    }

    #[test]
    fn skips_built_in_claude_runtime() {
        // Even if the user populated `runtimes.claude.supported_models` (the
        // built-in runtime's allowlist is additive, not strict), we never
        // *route* a model to it via derive — the built-in is already the
        // default fallback.
        let mut runtimes = HashMap::new();
        runtimes.insert("claude".to_string(), rt(&["shared-model"]));
        runtimes.insert("qwen-local".to_string(), rt(&["shared-model"]));
        assert_eq!(
            derive_runtime_from_model(Some("shared-model"), &runtimes),
            Some("qwen-local".to_string())
        );
    }

    #[test]
    fn deterministic_on_ambiguous_model() {
        // When two non-claude runtimes claim the same model, alphabetical
        // order on runtime name wins so subsequent calls are stable.
        let mut runtimes = HashMap::new();
        runtimes.insert("zeta".to_string(), rt(&["dup"]));
        runtimes.insert("alpha".to_string(), rt(&["dup"]));
        assert_eq!(
            derive_runtime_from_model(Some("dup"), &runtimes),
            Some("alpha".to_string())
        );
    }
}
