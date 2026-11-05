use std::sync::{Arc, Mutex};

use runkon_flow::engine_error::EngineError;

use super::bridge_lock_err;
use super::turn_completion;
use runkon_runtimes::RuntimeResolver as _;

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

        let result = self.run_attempt(ctx, info, params, child_run_id.clone());

        // Add this attempt's usage onto the step row on every exit path: a failed
        // attempt still cost tokens. Runs after `persist_corrected_run_metrics`
        // (inside `run_attempt`), so the agent run row holds the correction totals.
        if !info.step_id.is_empty() {
            match self.conn.lock() {
                Ok(conn) => {
                    if let Err(e) = crate::workflow::add_step_metrics_from_agent_run(
                        &conn,
                        &info.step_id,
                        &child_run_id,
                    ) {
                        tracing::warn!(
                            "step '{}' (step_id={}): failed to add usage of run {child_run_id} to the step: {e}",
                            params.name,
                            info.step_id,
                        );
                    }
                }
                Err(e) => tracing::warn!(
                    "step '{}': db lock poisoned, skipping step usage for run {child_run_id}: {e}",
                    params.name,
                ),
            }
        }
        result
    }
}

impl RkActionExecutorAdapter {
    /// Run one agent attempt against the pre-created `child_run_id` agent run.
    fn run_attempt(
        &self,
        ctx: &dyn runkon_flow::traits::run_context::RunContext,
        info: &runkon_flow::traits::action_executor::StepInfo,
        params: &runkon_flow::traits::action_executor::ActionParams,
        child_run_id: String,
    ) -> Result<runkon_flow::traits::action_executor::ActionOutput, EngineError> {
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

        // For the unfinished-turn check (#3177): the agent definition (for
        // `can_commit` and the resume request) and whether the tree was clean
        // before the step, so a dirty tree inherited from earlier is not blamed
        // on the agent. A load error is left for `inner.execute` to report.
        let working_dir = ctx.working_dir().to_path_buf();
        let agent_def = runkon_flow_executors::agent_loader::load_agent(
            &working_dir.to_string_lossy(),
            &ctx.get(crate::workflow::engine_keys::REPO_PATH)
                .unwrap_or_default(),
            &params.name,
            Some(ctx.workflow_name()),
            &params.plugin_dirs,
            self.config.general.default_runtime.as_deref(),
        )
        .map_err(|e| {
            tracing::warn!(
                "step '{}': cannot load agent for the unfinished-turn check, skipping it: {e}",
                params.name
            );
        })
        .ok();
        let tree_clean_before = !params.dry_run
            && agent_def.as_ref().is_some_and(|d| d.can_commit)
            && turn_completion::tracked_changes(&working_dir).is_some_and(|f| f.is_empty());
        let resume_resolver = resolver.clone();
        let resume_host = host_adapter.clone();
        let resume_runtime = runtime_override.clone();

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
        if let Some(agent_def) = agent_def {
            let log_path = crate::config::agent_log_path(&child_run_id)
                .unwrap_or_else(|_| std::env::temp_dir().join(format!("{child_run_id}.log")));
            let schema = schema_arc.as_deref();
            turn_completion::finish_unfinished_turn(
                &params.name,
                &mut output,
                schema,
                || turn_completion::assess(&log_path, &working_dir, tree_clean_before),
                || {
                    let session = {
                        crate::agent::log_parsing::read_log_result(&log_path)
                            .and_then(|r| r.session_id)
                            .or_else(|| {
                                let conn = self
                                    .conn
                                    .lock()
                                    .map_err(|e| {
                                        tracing::warn!(
                                            "step '{}': db lock poisoned, cannot look up session for run {child_run_id}: {e}",
                                            params.name
                                        );
                                    })
                                    .ok()?;
                                match crate::agent::AgentManager::new(&conn).get_run(&child_run_id) {
                                    Ok(run) => run.and_then(|r| r.session_id),
                                    Err(e) => {
                                        tracing::warn!(
                                            "step '{}': cannot look up session for run {child_run_id}: {e}",
                                            params.name
                                        );
                                        None
                                    }
                                }
                            })
                    };
                    // A session a human took over interactively (#3163) must not be
                    // resumed headless: `None` makes the step report its unfinished turn.
                    headless_resumable_session(&self.conn, &params.name, &child_run_id, session)
                },
                |prompt, session_id| {
                    let effective_runtime = resume_runtime
                        .clone()
                        .unwrap_or_else(|| agent_def.runtime.clone());
                    let runtime = resume_resolver.resolve(&effective_runtime).map_err(|e| {
                        format!("failed to resolve runtime '{effective_runtime}': {e}")
                    })?;
                    let request = build_resume_request(ResumeRequestParts {
                        run_id: &child_run_id,
                        effective_runtime,
                        agent_def: &agent_def,
                        prompt,
                        working_dir: &working_dir,
                        params,
                        session_id,
                        host: &resume_host,
                    });
                    runkon_flow_executors::correction::run_agent_with_output_correction(
                        &*runtime,
                        request,
                        ctx.shutdown(),
                        info.step_timeout,
                        schema,
                    )
                },
            )?;
        }
        if output
            .metadata
            .contains_key(runkon_flow::constants::metadata_keys::OUTPUT_CORRECTION_ATTEMPTS)
            || output
                .metadata
                .contains_key(turn_completion::FINISH_TURN_ATTEMPTS)
        {
            // Runkon summed the metrics across correction attempts; the row only
            // holds the final attempt's, so write the totals back (best-effort).
            match self.conn.lock() {
                Ok(conn) => {
                    if let Err(e) =
                        persist_corrected_run_metrics(&conn, &child_run_id, &output.metadata)
                    {
                        tracing::warn!(
                            "step '{}': failed to persist summed correction metrics for run {}: {e}",
                            params.name,
                            child_run_id,
                        );
                    }
                }
                Err(e) => tracing::warn!(
                    "step '{}': db lock poisoned, skipping summed correction metrics for run {}: {e}",
                    params.name,
                    child_run_id,
                ),
            }
        }
        if let Some(e) = schema_output_missing(&params.name, schema_arc.is_some(), &output.metadata)
        {
            return Err(e);
        }
        output.child_run_id = Some(child_run_id);
        Ok(output)
    }
}

struct ResumeRequestParts<'a> {
    run_id: &'a str,
    effective_runtime: String,
    agent_def: &'a runkon_runtimes::agent_def::AgentDef,
    prompt: &'a str,
    working_dir: &'a std::path::Path,
    params: &'a runkon_flow::traits::action_executor::ActionParams,
    session_id: &'a str,
    host: &'a Arc<crate::runtime::adapter::SqliteHostAdapter>,
}

/// Build the `RuntimeRequest` for resuming a finished run's session. The
/// first-run request is built inside `ClaudeAgentExecutor` (runkon-anthropic),
/// so the conductor-side pieces — `--bot-name`, plugin dirs, tracker and event
/// sink — are kept together here; keep them in step with that executor.
fn build_resume_request(p: ResumeRequestParts<'_>) -> runkon_runtimes::RuntimeRequest {
    let extra_cli_args = match &p.params.as_identity {
        Some(name) => vec![(
            std::borrow::Cow::Borrowed("--bot-name"),
            std::borrow::Cow::Owned(name.clone()),
        )],
        None => vec![],
    };
    runkon_runtimes::RuntimeRequest {
        run_id: p.run_id.to_string(),
        effective_runtime: p.effective_runtime,
        agent_def: p.agent_def.clone(),
        prompt: p.prompt.to_string(),
        working_dir: p.working_dir.to_path_buf(),
        model: p.params.model.clone(),
        extra_cli_args,
        plugin_dirs: p.params.plugin_dirs.clone(),
        resume_session_id: Some(p.session_id.to_string()),
        tracker: p.host.clone() as Arc<dyn runkon_runtimes::RunTracker>,
        event_sink: p.host.clone() as Arc<dyn runkon_runtimes::RunEventSink>,
    }
}

/// Fail a step that has an output schema but whose agent produced no usable
/// output. runkon 0.7.1 fails a schema step on a missing or invalid
/// `FLOW_OUTPUT` block, but still passes it with empty markers when the agent
/// produced no result text at all; it only records `OUTPUT_WARNING`. For
/// `verify` that reads as "no `has_failures`", i.e. a pass.
///
/// Steps without a schema keep the warning-only behaviour. This can go once
/// runkon/runkon#105 (engine-level `require_output`) lands.
fn schema_output_missing(
    step: &str,
    has_schema: bool,
    metadata: &std::collections::HashMap<String, String>,
) -> Option<EngineError> {
    if !has_schema {
        return None;
    }
    let warning = metadata.get(runkon_flow::constants::metadata_keys::OUTPUT_WARNING)?;
    Some(EngineError::Workflow(format!(
        "step '{step}': has an output schema but produced no usable output: {warning}"
    )))
}

/// `session` when the finish-turn path may resume it headless: `None` when there is no session,
/// a human took it over interactively (#3163), or ownership can't be determined (a `None`
/// makes the step report its unfinished turn rather than resume blindly).
fn headless_resumable_session(
    conn: &std::sync::Mutex<rusqlite::Connection>,
    step: &str,
    child_run_id: &str,
    session: Option<String>,
) -> Option<String> {
    let sid = session?;
    let conn = conn
        .lock()
        .map_err(|e| {
            tracing::warn!(
                "step '{step}': db lock poisoned, cannot check session ownership for run {child_run_id}: {e}"
            );
        })
        .ok()?;
    match crate::session::takeover::resumable_session_id(&conn, Some(&sid)) {
        Ok(resumable) => {
            if resumable.is_none() {
                tracing::warn!(
                    "step '{step}': session {sid} was taken over interactively; not resuming it to finish the turn"
                );
            }
            resumable
        }
        Err(e) => {
            tracing::warn!(
                "step '{step}': cannot check session ownership for run {child_run_id}: {e}"
            );
            None
        }
    }
}

/// Write the metrics runkon summed across output-correction attempts onto the
/// step's `agent_runs` row. Missing or unparseable keys leave that column as is.
fn persist_corrected_run_metrics(
    conn: &rusqlite::Connection,
    run_id: &str,
    metadata: &std::collections::HashMap<String, String>,
) -> crate::error::Result<()> {
    use runkon_flow::constants::metadata_keys as k;
    fn get<T: std::str::FromStr>(
        m: &std::collections::HashMap<String, String>,
        key: &str,
    ) -> Option<T> {
        m.get(key).and_then(|v| v.parse().ok())
    }
    let metrics = crate::agent::RunMetrics {
        cost_usd: get(metadata, k::COST_USD),
        num_turns: get(metadata, k::NUM_TURNS),
        duration_ms: get(metadata, k::DURATION_MS),
        input_tokens: get(metadata, k::INPUT_TOKENS),
        output_tokens: get(metadata, k::OUTPUT_TOKENS),
        cache_read_input_tokens: get(metadata, k::CACHE_READ_INPUT_TOKENS),
        cache_creation_input_tokens: get(metadata, k::CACHE_CREATION_INPUT_TOKENS),
    };
    crate::agent::AgentManager::new(conn).set_run_metrics(run_id, &metrics)
}

#[cfg(test)]
mod tests {
    use super::{
        derive_runtime_from_model, headless_resumable_session, persist_corrected_run_metrics,
        schema_output_missing,
    };
    use runkon_runtimes::config::RuntimeConfig;
    use std::collections::HashMap;

    fn warning_metadata() -> HashMap<String, String> {
        let mut md = HashMap::new();
        md.insert(
            runkon_flow::constants::metadata_keys::OUTPUT_WARNING.to_string(),
            "no FLOW_OUTPUT block".to_string(),
        );
        md
    }

    #[test]
    fn schema_step_with_output_warning_fails() {
        let err = schema_output_missing("verify", true, &warning_metadata())
            .expect("schema + warning must fail");
        let msg = err.to_string();
        assert!(msg.contains("verify"), "{msg}");
        assert!(msg.contains("no FLOW_OUTPUT block"), "{msg}");
    }

    #[test]
    fn schema_step_without_warning_passes() {
        assert!(schema_output_missing("verify", true, &HashMap::new()).is_none());
    }

    #[test]
    fn non_schema_step_with_output_warning_passes() {
        assert!(schema_output_missing("implement", false, &warning_metadata()).is_none());
    }

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

    #[test]
    fn persist_corrected_run_metrics_writes_summed_totals() {
        let conn = crate::test_helpers::setup_db();
        let mgr = crate::agent::AgentManager::new(&conn);
        let run = mgr.create_run(Some("w1"), "task", None).unwrap();

        let mut md = HashMap::new();
        md.insert("cost_usd".to_string(), "0.75".to_string());
        md.insert("num_turns".to_string(), "9".to_string());
        md.insert("input_tokens".to_string(), "1200".to_string());
        md.insert("output_tokens".to_string(), "bogus".to_string());
        md.insert("output_correction_attempts".to_string(), "1".to_string());
        persist_corrected_run_metrics(&conn, &run.id, &md).unwrap();

        let row = mgr.get_run(&run.id).unwrap().unwrap();
        assert_eq!(row.cost_usd, Some(0.75));
        assert_eq!(row.num_turns, Some(9));
        assert_eq!(row.input_tokens, Some(1200));
        assert_eq!(row.output_tokens, None);
    }

    fn conn_with_headless_run(session: &str) -> std::sync::Mutex<rusqlite::Connection> {
        use crate::test_helpers::{
            create_test_conn, insert_test_agent_run_with, insert_test_repo, insert_test_worktree,
            TestAgentRun,
        };
        let conn = create_test_conn();
        insert_test_repo(&conn, "r1", "myrepo", "/nonexistent/myrepo");
        insert_test_worktree(&conn, "w1", "r1", "fix-1-thing", "/tmp/wt/fix-1-thing");
        insert_test_agent_run_with(
            &conn,
            &TestAgentRun {
                id: "h1",
                worktree_id: Some("w1"),
                session_id: Some(session),
                status: "completed",
                runtime: "claude",
                parent_run_id: None,
                started_at: "2024-01-01T00:00:00Z",
            },
        );
        std::sync::Mutex::new(conn)
    }

    #[test]
    fn finish_turn_resumes_session_nobody_took_over() {
        let conn = conn_with_headless_run("sess-1");
        assert_eq!(
            headless_resumable_session(&conn, "step", "h1", Some("sess-1".into())).as_deref(),
            Some("sess-1")
        );
        assert_eq!(headless_resumable_session(&conn, "step", "h1", None), None);
    }

    #[test]
    fn finish_turn_never_resumes_a_taken_over_session() {
        let conn = conn_with_headless_run("sess-1");
        crate::test_helpers::insert_test_agent_run_with(
            &conn.lock().unwrap(),
            &crate::test_helpers::TestAgentRun {
                id: "i1",
                worktree_id: Some("w1"),
                session_id: Some("sess-1"),
                status: "idle",
                runtime: "interactive",
                parent_run_id: Some("h1"),
                started_at: "2024-01-02T00:00:00Z",
            },
        );
        assert_eq!(
            headless_resumable_session(&conn, "step", "h1", Some("sess-1".into())),
            None
        );
    }

    #[test]
    fn finish_turn_does_not_resume_when_ownership_check_fails() {
        let conn = conn_with_headless_run("sess-1");
        conn.lock()
            .unwrap()
            .execute_batch("DROP TABLE agent_runs")
            .unwrap();
        assert_eq!(
            headless_resumable_session(&conn, "step", "h1", Some("sess-1".into())),
            None
        );
    }
}
