//! Workflow coordinator: execute and resume workflow definitions via `runkon_flow::FlowEngine`.
//!
//! Contains the public entry-points (`execute_workflow_standalone`, `resume_workflow`, …) and
//! the private helpers they share.  The old conductor-core execution stack (`engine.rs`) has
//! been removed; this module is the canonical home for workflow orchestration logic.

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::Connection;

use crate::agent::AgentManager;
use crate::agent_config::AgentSpec;
use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::schema_config::SchemaIssue;
use crate::worktree::WorktreeManager;
use runkon_flow::dsl::WorkflowDef;

use super::manager::WorkflowManager;
use super::status::{WorkflowRunStatus, WorkflowStepStatus};
use super::types::{
    SpawnHeartbeatResumeParams, WorkflowExecStandalone, WorkflowResult, WorkflowResumeInput,
    WorkflowResumeStandalone,
};

/// Input keys that the workflow engine injects automatically from the run context
/// (ticket and repo metadata). Consumers can use this slice to identify inputs
/// that are read-only from the user's perspective.
///
/// Canonical definition lives in `runkon_flow::engine::ENGINE_INJECTED_KEYS`.
pub(crate) use runkon_flow::ENGINE_INJECTED_KEYS;

/// Validate required workflow inputs are present and apply default values.
///
/// Returns an error if a required input is missing.
pub fn apply_workflow_input_defaults(
    workflow: &WorkflowDef,
    inputs: &mut HashMap<String, String>,
) -> Result<()> {
    use runkon_flow::dsl::InputType;
    for input_decl in &workflow.inputs {
        if input_decl.required && !inputs.contains_key(&input_decl.name) {
            return Err(ConductorError::Workflow(format!(
                "Missing required input: '{}'. Use --input {}=<value>.",
                input_decl.name, input_decl.name
            )));
        }
        if let Some(ref default) = input_decl.default {
            inputs
                .entry(input_decl.name.clone())
                .or_insert_with(|| default.clone());
        }
        // Boolean inputs default to "false" when absent.
        if input_decl.input_type == InputType::Boolean {
            inputs
                .entry(input_decl.name.clone())
                .or_insert_with(|| "false".to_string());
        }
    }
    Ok(())
}

/// Validate that all agent definitions, prompt snippets, and output schemas
/// referenced in the workflow are resolvable from the given directories.
///
/// Called by both `execute_workflow` and `execute_workflow_standalone` before
/// any DB writes, so the function must be idempotent and free of side effects.
fn validate_workflow_resources(
    workflow: &WorkflowDef,
    working_dir: &str,
    repo_path: &str,
    extra_plugin_dirs: &[String],
) -> Result<()> {
    let mut all_agents = runkon_flow::dsl::collect_agent_names(&workflow.body);
    all_agents.extend(runkon_flow::dsl::collect_agent_names(&workflow.always));
    all_agents.sort();
    all_agents.dedup();

    let specs: Vec<AgentSpec> = all_agents.iter().map(AgentSpec::from).collect();
    let mut all_plugin_dirs = extra_plugin_dirs.to_vec();
    let mut seen: std::collections::HashSet<String> = all_plugin_dirs.iter().cloned().collect();
    for dir in workflow.collect_all_plugin_dirs() {
        if seen.insert(dir.clone()) {
            all_plugin_dirs.push(dir);
        }
    }
    let missing_agents = crate::agent_config::find_missing_agents(
        working_dir,
        repo_path,
        &specs,
        Some(&workflow.name),
        &all_plugin_dirs,
    );
    if !missing_agents.is_empty() {
        return Err(ConductorError::Workflow(format!(
            "Missing agent definitions: {}. Run 'conductor workflow validate' for details.",
            missing_agents.join(", ")
        )));
    }

    let all_snippets = workflow.collect_all_snippet_refs();
    if !all_snippets.is_empty() {
        let missing_snippets = crate::prompt_config::find_missing_snippets(
            working_dir,
            repo_path,
            &all_snippets,
            Some(&workflow.name),
        );
        if !missing_snippets.is_empty() {
            return Err(ConductorError::Workflow(format!(
                "Missing prompt snippets: {}. Check .conductor/prompts/ directory.",
                missing_snippets.join(", ")
            )));
        }
    }

    let all_schemas = workflow.collect_all_schema_refs();
    if !all_schemas.is_empty() {
        let schema_issues = crate::schema_config::check_schemas(
            working_dir,
            repo_path,
            &all_schemas,
            Some(&workflow.name),
        );
        if !schema_issues.is_empty() {
            let details: Vec<String> = schema_issues
                .iter()
                .map(|issue| match issue {
                    SchemaIssue::Missing(name) => format!("missing: {name}"),
                    SchemaIssue::Invalid { name, error } => {
                        format!("invalid: {name}: {error}")
                    }
                })
                .collect();
            return Err(ConductorError::Workflow(format!(
                "Schema validation failed: {}",
                details.join(", ")
            )));
        }
    }

    Ok(())
}

/// Insert `value` under `key` only when the key is absent — existing caller-supplied
/// values are never overwritten. All inject_*_variables functions use this helper.
fn set_input(inputs: &mut HashMap<String, String>, key: &str, value: String) {
    inputs.entry(key.to_string()).or_insert(value);
}

fn inject_worktree_variables(
    wt: &crate::worktree::Worktree,
    repo_default_branch: &str,
    merged_inputs: &mut HashMap<String, String>,
) {
    let base = wt.effective_base(repo_default_branch);
    set_input(merged_inputs, "feature_base_branch", base.to_string());
    set_input(merged_inputs, "worktree_branch", wt.branch.clone());
}

fn inject_ticket_variables(
    ticket: &crate::tickets::Ticket,
    merged_inputs: &mut HashMap<String, String>,
) {
    set_input(merged_inputs, "ticket_id", ticket.id.clone());
    set_input(merged_inputs, "ticket_source_id", ticket.source_id.clone());
    set_input(
        merged_inputs,
        "ticket_source_type",
        ticket.source_type.clone(),
    );
    set_input(merged_inputs, "ticket_title", ticket.title.clone());
    set_input(merged_inputs, "ticket_body", ticket.body.clone());
    set_input(merged_inputs, "ticket_url", ticket.url.clone());
    set_input(merged_inputs, "ticket_raw_json", ticket.raw_json.clone());
}

fn inject_repo_variables(repo: &crate::repo::Repo, merged_inputs: &mut HashMap<String, String>) {
    set_input(merged_inputs, "repo_id", repo.id.clone());
    set_input(merged_inputs, "repo_path", repo.local_path.clone());
    set_input(merged_inputs, "repo_name", repo.slug.clone());
}

fn deserialize_workflow_snapshot(snapshot: &str) -> Result<runkon_flow::dsl::WorkflowDef> {
    serde_json::from_str(snapshot).map_err(|e| {
        ConductorError::Workflow(format!("Failed to deserialize workflow definition: {e}"))
    })
}

/// Acquire the shared SQLite connection mutex, mapping a poison error to a `ConductorError`.
fn lock_shared(
    conn: &Arc<std::sync::Mutex<Connection>>,
) -> Result<std::sync::MutexGuard<'_, Connection>> {
    conn.lock()
        .map_err(|e| ConductorError::Workflow(format!("db mutex poisoned: {e}")))
}

/// Guard for active runs at depth 0.
///
/// Returns `Ok(())` when no active run is found, or when the active run is cancelled
/// because `force = true`.  Returns `Err(WorkflowRunAlreadyActive)` when an active
/// run exists and `force = false`.
///
/// Extracted to a standalone function so it can be tested in isolation against an
/// in-memory database without setting up a full engine.
pub(crate) fn guard_active_run(
    wf_mgr: &WorkflowManager<'_>,
    worktree_id: &str,
    force: bool,
) -> Result<()> {
    if let Some(active) = wf_mgr.get_active_run_for_worktree(worktree_id)? {
        if force {
            tracing::info!(
                "Force override: cancelling active run {} to start new run",
                active.id
            );
            wf_mgr.cancel_run(&active.id, "force override: new run requested")?;
        } else {
            return Err(ConductorError::WorkflowRunAlreadyActive {
                name: active.workflow_name,
            });
        }
    }
    Ok(())
}

/// Common components shared between `execute_workflow_standalone` and `resume_workflow`.
///
/// Built once by [`build_rk_engine_components`] and consumed into `ExecutionState`.
struct RkEngineComponents {
    persistence: Arc<dyn runkon_flow::traits::persistence::WorkflowPersistence>,
    action_registry: Arc<runkon_flow::traits::action_executor::ActionRegistry>,
    child_runner: Arc<dyn runkon_flow::engine::ChildWorkflowRunner>,
}

/// Variable-field arguments for constructing a fresh [`runkon_flow::engine::ExecutionState`].
///
/// Passed to [`build_rk_execution_state`] which centralises the zero-valued fields so
/// they cannot diverge between the execute and resume paths.
struct RkStateArgs {
    persistence: Arc<dyn runkon_flow::traits::persistence::WorkflowPersistence>,
    action_registry: Arc<runkon_flow::traits::action_executor::ActionRegistry>,
    script_env_provider: Arc<dyn runkon_flow::ScriptEnvProvider>,
    workflow_run_id: String,
    workflow_name: String,
    worktree_ctx: runkon_flow::engine::WorktreeContext,
    model: Option<String>,
    exec_config: runkon_flow::types::WorkflowExecConfig,
    inputs: HashMap<String, String>,
    parent_run_id: String,
    depth: u32,
    target_label: Option<String>,
    default_bot_name: Option<String>,
    triggered_by_hook: bool,
    #[allow(clippy::type_complexity)]
    schema_resolver: Arc<
        dyn Fn(
                &str,
                &str,
                &str,
            )
                -> runkon_flow::engine_error::Result<runkon_flow::output_schema::OutputSchema>
            + Send
            + Sync,
    >,
    child_runner: Arc<dyn runkon_flow::engine::ChildWorkflowRunner>,
    registry: Arc<runkon_flow::ItemProviderRegistry>,
    event_sinks: Arc<[Arc<dyn runkon_flow::EventSink>]>,
}

/// Build a fresh [`runkon_flow::engine::ExecutionState`] from the variable arguments,
/// filling in all zero-initialised metric and runtime fields in one place so the
/// execute and resume paths cannot diverge silently.
fn build_rk_execution_state(args: RkStateArgs) -> runkon_flow::engine::ExecutionState {
    runkon_flow::engine::ExecutionState {
        persistence: args.persistence,
        action_registry: args.action_registry,
        script_env_provider: args.script_env_provider,
        workflow_run_id: args.workflow_run_id,
        workflow_name: args.workflow_name,
        worktree_ctx: args.worktree_ctx,
        model: args.model,
        exec_config: args.exec_config,
        inputs: args.inputs,
        parent_run_id: args.parent_run_id,
        depth: args.depth,
        target_label: args.target_label,
        step_results: HashMap::new(),
        contexts: Vec::new(),
        position: 0,
        all_succeeded: true,
        total_cost: 0.0,
        total_turns: 0,
        total_duration_ms: 0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_input_tokens: 0,
        total_cache_creation_input_tokens: 0,
        last_gate_feedback: None,
        block_output: None,
        block_with: Vec::new(),
        resume_ctx: None,
        default_bot_name: args.default_bot_name,
        triggered_by_hook: args.triggered_by_hook,
        schema_resolver: Some(args.schema_resolver),
        child_runner: Some(args.child_runner),
        last_heartbeat_at: runkon_flow::engine::ExecutionState::new_heartbeat(),
        registry: args.registry,
        event_sinks: args.event_sinks,
        cancellation: runkon_flow::CancellationToken::new(),
        current_execution_id: Arc::new(std::sync::Mutex::new(None)),
    }
}

/// Build a [`runkon_flow::FlowEngine`] with all gate resolvers registered.
fn build_flow_engine(
    persistence: &Arc<dyn runkon_flow::traits::persistence::WorkflowPersistence>,
    event_sinks: &Arc<[Arc<dyn runkon_flow::EventSink>]>,
    working_dir: String,
    default_bot_name: Option<String>,
    config: Config,
    db: &std::path::Path,
    workflow_name: &str,
) -> Result<runkon_flow::FlowEngine> {
    super::runkon_gate_bridge::register_rk_gate_resolvers(
        runkon_flow::FlowEngineBuilder::new().with_event_sinks(event_sinks),
        Arc::clone(persistence),
        working_dir,
        default_bot_name,
        config,
        db.to_path_buf(),
    )
    .build()
    .map_err(|e| {
        ConductorError::Workflow(format!("failed to build engine for '{workflow_name}': {e}"))
    })
}

/// Map a [`runkon_flow::engine_error::EngineError`] to [`ConductorError`].
///
/// `phase` is a short label inserted into the error message (e.g. `"run"` or `"resume"`).
fn map_engine_error(
    e: runkon_flow::engine_error::EngineError,
    workflow_name: &str,
    phase: &str,
) -> ConductorError {
    match e {
        runkon_flow::engine_error::EngineError::Cancelled(_) => ConductorError::WorkflowCancelled,
        other => ConductorError::Workflow(format!(
            "workflow '{workflow_name}' {phase} failed: {other}"
        )),
    }
}

/// Build the persistence, action-registry, and child-runner that are identical
/// between fresh execution and resume.  Call sites then layer in the parts that
/// differ (item_registry, script_env_provider, ExecutionState fields).
fn build_rk_engine_components(
    config: &crate::config::Config,
    shared_conn: &Arc<std::sync::Mutex<Connection>>,
    db: &std::path::Path,
) -> RkEngineComponents {
    let core_persistence = Arc::new(
        super::persistence_sqlite::SqliteWorkflowPersistence::from_shared_connection(Arc::clone(
            shared_conn,
        )),
    );
    let persistence: Arc<dyn runkon_flow::traits::persistence::WorkflowPersistence> =
        Arc::new(super::runkon_bridge::PersistenceAdapter(core_persistence));
    let action_registry = Arc::new(super::runkon_bridge::build_rk_action_registry(
        config,
        Arc::clone(shared_conn),
        db,
    ));
    let child_runner: Arc<dyn runkon_flow::engine::ChildWorkflowRunner> =
        Arc::new(super::runkon_bridge::ConductorChildWorkflowRunner::new(
            db.to_path_buf(),
            config.clone(),
            Arc::clone(shared_conn),
        ));
    RkEngineComponents {
        persistence,
        action_registry,
        child_runner,
    }
}

/// Build a schema resolver closure for the given workflow name.
#[allow(clippy::type_complexity)]
fn make_schema_resolver(
    workflow_name: String,
) -> Arc<
    dyn Fn(
            &str,
            &str,
            &str,
        ) -> runkon_flow::engine_error::Result<runkon_flow::output_schema::OutputSchema>
        + Send
        + Sync,
> {
    Arc::new(move |working_dir, repo_path, name| {
        let schema_ref = crate::schema_config::SchemaRef::from_str_value(name);
        crate::schema_config::load_schema(working_dir, repo_path, &schema_ref, Some(&workflow_name))
            .map(super::runkon_bridge::core_schema_to_rk)
            .map_err(|e| runkon_flow::engine_error::EngineError::Workflow(e.to_string()))
    })
}

/// Execute a workflow in a self-contained manner using `runkon_flow::FlowEngine::run()`.
///
/// Opens its own database connection and builds all bridge adapters so the
/// caller does not need to share a `&Connection` or know about internal engine
/// types.  Designed for use in background threads (TUI, web, CLI sub-commands).
pub fn execute_workflow_standalone(params: &WorkflowExecStandalone) -> Result<WorkflowResult> {
    let db = params
        .db_path
        .clone()
        .unwrap_or_else(crate::config::db_path);

    let raw_conn = crate::db::open_database(&db)?;
    let shared_conn = Arc::new(std::sync::Mutex::new(raw_conn));

    let config = &params.config;
    let workflow = &params.workflow;

    // -----------------------------------------------------------------------
    // Setup phase — acquire lock once, do all pre-run work, release.
    // -----------------------------------------------------------------------
    let (wf_run_id, parent_run_id, merged_inputs, effective_repo_id_owned, snapshot_json) = {
        let guard = lock_shared(&shared_conn)?;
        let conn: &Connection = &guard;

        let agent_mgr = AgentManager::new(conn);
        let wf_mgr = WorkflowManager::new(conn);

        // Validate agents, snippets, and schemas referenced by this workflow.
        validate_workflow_resources(
            workflow,
            &params.working_dir,
            &params.repo_path,
            &params.extra_plugin_dirs,
        )?;

        // Snapshot the definition.
        let snapshot_json = serde_json::to_string(workflow).map_err(|e| {
            ConductorError::Workflow(format!("Failed to serialize workflow definition: {e}"))
        })?;

        // Guard active runs at depth 0 only — child workflows (depth > 0) run
        // concurrently with their parent and must not trigger this check.
        if params.depth == 0 {
            if let Some(ref wt_id) = params.worktree_id {
                guard_active_run(&wf_mgr, wt_id, params.force)?;
            }
        }

        // Create parent agent run.
        let parent_prompt = format!("Workflow: {} — {}", workflow.name, workflow.description);
        let parent_run = agent_mgr.create_run(
            params.worktree_id.as_deref(),
            &parent_prompt,
            params.model.as_deref(),
        )?;

        // Derive repo_id from worktree when not explicitly provided.
        // Cache the fetched Worktree to avoid a second DB lookup during variable injection below.
        let mut fetched_worktree: Option<crate::worktree::Worktree> = None;
        let derived_repo_id = match (&params.repo_id, &params.worktree_id) {
            (None, Some(wt_id)) => {
                match crate::worktree::WorktreeManager::new(conn, config).get_by_id(wt_id) {
                    Ok(wt) => {
                        let repo_id = wt.repo_id.clone();
                        fetched_worktree = Some(wt);
                        Some(repo_id)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to look up worktree '{wt_id}' for repo_id derivation: {e}"
                        );
                        None
                    }
                }
            }
            _ => None,
        };
        let effective_repo_id: Option<String> = params.repo_id.clone().or(derived_repo_id);

        let trigger_str = if params.triggered_by_hook {
            "hook".to_string()
        } else {
            workflow.trigger.to_string()
        };

        let wf_run = wf_mgr.create_workflow_run_with_targets(
            &workflow.name,
            params.worktree_id.as_deref(),
            params.ticket_id.as_deref(),
            effective_repo_id.as_deref(),
            &parent_run.id,
            params.exec_config.dry_run,
            &trigger_str,
            Some(&snapshot_json),
            params.parent_workflow_run_id.as_deref(),
            params.target_label.as_deref(),
        )?;

        // Write child run ID back to parent step immediately so TUI can drill in while running.
        if let Some(ref step_id) = params.parent_step_id {
            wf_mgr.update_step_child_run_id(step_id, &wf_run.id)?;
        }

        // Notify any waiting caller of the freshly-created run ID.
        if let Some(pair) = &params.run_id_notify {
            let (lock, cvar) = pair.as_ref();
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = Some(wf_run.id.clone());
            cvar.notify_one();
        }

        // Persist default_bot_name so it can be restored on resume.
        if let Some(ref bot_name) = params.default_bot_name {
            wf_mgr.set_workflow_run_default_bot_name(&wf_run.id, bot_name)?;
        }

        // Persist loop iteration number for sub-workflow runs.
        if params.iteration > 0 {
            wf_mgr.set_workflow_run_iteration(&wf_run.id, params.iteration as i64)?;
        }

        // Build merged inputs, injecting ticket/repo/worktree variables.
        let mut merged_inputs = params.inputs.clone();
        if let Some(ref tid) = params.ticket_id {
            let ticket = crate::tickets::TicketSyncer::new(conn).get_by_id(tid)?;
            inject_ticket_variables(&ticket, &mut merged_inputs);
        }
        let fetched_repo = if let Some(ref rid) = effective_repo_id {
            let repo = crate::repo::RepoManager::new(conn, config).get_by_id(rid)?;
            inject_repo_variables(&repo, &mut merged_inputs);
            Some(repo)
        } else {
            None
        };
        if let Some(ref wt_id) = params.worktree_id {
            // Reuse the Worktree cached during repo_id derivation (the common case where
            // effective_repo_id came from this same worktree), or fetch it now if needed.
            let wt = match fetched_worktree {
                Some(cached) => cached,
                None => crate::worktree::WorktreeManager::new(conn, config).get_by_id(wt_id)?,
            };
            let default_branch = if let Some(ref r) = fetched_repo {
                r.default_branch.clone()
            } else {
                crate::repo::RepoManager::new(conn, config)
                    .get_by_id(&wt.repo_id)?
                    .default_branch
            };
            inject_worktree_variables(&wt, &default_branch, &mut merged_inputs);
        }

        // Persist inputs.
        if !merged_inputs.is_empty() {
            wf_mgr.set_workflow_run_inputs(&wf_run.id, &merged_inputs)?;
        }

        // Mark as running.
        wf_mgr.update_workflow_status(&wf_run.id, WorkflowRunStatus::Running, None, None)?;

        (
            wf_run.id,
            parent_run.id,
            merged_inputs,
            effective_repo_id,
            snapshot_json,
        )
        // guard drops here — connection lock released
    };

    // -----------------------------------------------------------------------
    // Build the runkon-flow engine and execution state.
    // -----------------------------------------------------------------------

    // Reuse the snapshot JSON already computed during setup to avoid a second serialization.
    let rk_def = deserialize_workflow_snapshot(&snapshot_json)?;

    let RkEngineComponents {
        persistence,
        action_registry,
        child_runner,
    } = build_rk_engine_components(config, &shared_conn, &db);

    let item_registry = Arc::new(super::runkon_bridge::build_rk_item_provider_registry(
        Arc::clone(&shared_conn),
        config,
        effective_repo_id_owned.clone(),
    ));

    let script_env_provider = super::runkon_bridge::build_rk_script_env_provider(
        params.conductor_bin_dir.clone(),
        params.extra_plugin_dirs.clone(),
    );

    let schema_resolver = make_schema_resolver(workflow.name.clone());

    let rk_exec_config = runkon_flow::types::WorkflowExecConfig {
        poll_interval: params.exec_config.poll_interval,
        step_timeout: params.exec_config.step_timeout,
        fail_fast: params.exec_config.fail_fast,
        dry_run: params.exec_config.dry_run,
        shutdown: params.exec_config.shutdown.clone(),
    };
    let event_sinks: Arc<[Arc<dyn runkon_flow::EventSink>]> =
        Arc::from(params.exec_config.event_sinks.clone());

    let mut rk_state = build_rk_execution_state(RkStateArgs {
        persistence: Arc::clone(&persistence),
        action_registry,
        script_env_provider,
        workflow_run_id: wf_run_id,
        workflow_name: workflow.name.clone(),
        worktree_ctx: runkon_flow::engine::WorktreeContext {
            worktree_id: params.worktree_id.clone(),
            working_dir: params.working_dir.clone(),
            repo_path: params.repo_path.clone(),
            ticket_id: params.ticket_id.clone(),
            repo_id: effective_repo_id_owned,
            extra_plugin_dirs: params.extra_plugin_dirs.clone(),
        },
        model: params.model.clone(),
        exec_config: rk_exec_config,
        inputs: merged_inputs,
        parent_run_id: parent_run_id.clone(),
        depth: params.depth,
        target_label: params.target_label.clone(),
        default_bot_name: params.default_bot_name.clone(),
        triggered_by_hook: params.triggered_by_hook,
        schema_resolver,
        child_runner,
        registry: item_registry,
        event_sinks: Arc::clone(&event_sinks),
    });

    let engine = build_flow_engine(
        &persistence,
        &event_sinks,
        params.working_dir.clone(),
        params.default_bot_name.clone(),
        config.clone(),
        &db,
        &workflow.name,
    )?;

    let rk_result = engine
        .run(&rk_def, &mut rk_state)
        .map_err(|e| map_engine_error(e, &workflow.name, "run"))?;

    // Close the parent agent run. It was created without a subprocess_pid (workflow
    // parent runs never spawn a subprocess), so the orphan reaper would sweep it the
    // moment the workflow_run becomes terminal unless we explicitly mark it done here.
    {
        let guard = lock_shared(&shared_conn)?;
        let agent_mgr = AgentManager::new(&guard);
        let summary = format!("Workflow '{}' completed", workflow.name);
        if rk_result.all_succeeded {
            if let Err(e) = agent_mgr.update_run_completed(
                &parent_run_id,
                None,
                Some(&summary),
                Some(rk_result.total_cost),
                Some(rk_result.total_turns),
                Some(rk_result.total_duration_ms),
                Some(rk_result.total_input_tokens),
                Some(rk_result.total_output_tokens),
                Some(rk_result.total_cache_read_input_tokens),
                Some(rk_result.total_cache_creation_input_tokens),
            ) {
                tracing::warn!("Failed to mark parent run {parent_run_id} completed: {e}");
            }
        } else if let Err(e) = agent_mgr.update_run_failed(
            &parent_run_id,
            &format!("Workflow '{}' failed", workflow.name),
        ) {
            tracing::warn!("Failed to mark parent run {parent_run_id} failed: {e}");
        }
    }

    Ok(rk_result.into())
}

/// Validate resume preconditions that can be checked from status alone.
///
/// Shared by the core `resume_workflow` function and the web endpoint so that
/// validation rules and error strings stay in a single place.
pub fn validate_resume_preconditions(
    status: &WorkflowRunStatus,
    restart: bool,
    from_step: Option<&str>,
) -> Result<()> {
    if matches!(status, WorkflowRunStatus::Completed) && !restart {
        return Err(ConductorError::Workflow(
            "Cannot resume a completed workflow run. Use --restart to re-run from the beginning."
                .to_string(),
        ));
    }
    if matches!(status, WorkflowRunStatus::Running) {
        return Err(ConductorError::Workflow(
            "Cannot resume a workflow run that is already running.".to_string(),
        ));
    }
    if matches!(status, WorkflowRunStatus::Cancelled) {
        return Err(ConductorError::Workflow(
            "Cannot resume a cancelled workflow run.".to_string(),
        ));
    }
    if restart && from_step.is_some() {
        return Err(ConductorError::Workflow(
            "Cannot use --restart and --from-step together: --restart re-runs all steps, \
             --from-step resumes from a specific step."
                .to_string(),
        ));
    }
    Ok(())
}

/// Resume a workflow in a self-contained manner: opens its own database
/// connection. Designed for use in background threads.
pub fn resume_workflow_standalone(params: &WorkflowResumeStandalone) -> Result<WorkflowResult> {
    let db = params
        .db_path
        .clone()
        .unwrap_or_else(crate::config::db_path);

    let input = WorkflowResumeInput {
        config: &params.config,
        workflow_run_id: &params.workflow_run_id,
        model: params.model.as_deref(),
        from_step: params.from_step.as_deref(),
        restart: params.restart,
        conductor_bin_dir: params.conductor_bin_dir.clone(),
        event_sinks: vec![],
        db_path: Some(db),
        shutdown: params.shutdown.clone(),
    };

    resume_workflow(&input)
}

/// Build the standard [`WorkflowResumeStandalone`] for a watchdog-spawned resume.
///
/// Centralises the five constant fields so `spawn_workflow_resume` and
/// `spawn_heartbeat_resume` share one construction path.
fn make_resume_params(
    config: Config,
    run_id: String,
    conductor_bin_dir: Option<std::path::PathBuf>,
    db_path: Option<std::path::PathBuf>,
) -> WorkflowResumeStandalone {
    WorkflowResumeStandalone {
        config,
        workflow_run_id: run_id,
        model: None,
        from_step: None,
        restart: false,
        db_path,
        conductor_bin_dir,
        shutdown: None,
    }
}

/// Spawn a background thread to resume a workflow run.
///
/// Designed for the TUI/CLI watchdog callers so they are not blocked and the
/// WorkflowManager (data-access layer) does not need to call engine-layer code.
///
/// Returns the thread `JoinHandle` so callers can optionally join for testing;
/// production callers may drop it to detach.
pub fn spawn_workflow_resume(
    run_id: String,
    config: Config,
    conductor_bin_dir: Option<std::path::PathBuf>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let params = make_resume_params(config, run_id.clone(), conductor_bin_dir, None);
        if let Err(e) = resume_workflow_standalone(&params) {
            tracing::warn!(run_id = %run_id, "spawn_workflow_resume: auto-resume failed: {e}");
        }
    })
}

/// Spawn a background thread to resume a heartbeat-stuck workflow run.
///
/// Fires `fire_heartbeat_stuck_failed_notification` on failure so callers do
/// not need to inline this notification logic.
///
/// Returns the thread `JoinHandle` so callers can optionally wait for
/// completion (production callers may drop it to detach).
pub fn spawn_heartbeat_resume(p: SpawnHeartbeatResumeParams) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let effective_db = p.db_path.clone().unwrap_or_else(crate::config::db_path);
        let params = make_resume_params(
            p.config.clone(),
            p.run_id.clone(),
            p.conductor_bin_dir,
            Some(effective_db.clone()),
        );
        if let Err(e) = resume_workflow_standalone(&params) {
            tracing::warn!(run_id = %p.run_id, "spawn_heartbeat_resume: auto-resume failed: {e}");
            match crate::db::open_database(&effective_db) {
                Ok(db) => {
                    crate::notify::fire_heartbeat_stuck_failed_notification(
                        &db,
                        &p.config.notifications,
                        &p.config.notify.hooks,
                        &p.run_id,
                        &p.workflow_name,
                        p.target_label.as_deref(),
                        &e.to_string(),
                    );
                }
                Err(db_err) => {
                    tracing::warn!(
                        run_id = %p.run_id,
                        error = %db_err,
                        "spawn_heartbeat_resume: could not open DB to fire stuck-run notification"
                    );
                }
            }
        }
    })
}

/// Spawn a resume thread for each claimed run ID.
///
/// Consolidates the claim-then-spawn loop used by TUI and CLI watchdogs so
/// the pattern lives in one place.
pub fn spawn_claimed_runs(
    claimed: Vec<String>,
    config: Config,
    conductor_bin_dir: Option<std::path::PathBuf>,
) {
    for run_id in claimed {
        spawn_workflow_resume(run_id, config.clone(), conductor_bin_dir.clone());
    }
}

/// Resume a failed or stalled workflow run from the point of failure.
///
/// Loads the workflow definition from the run's `definition_snapshot`, rebuilds
/// the skip set from completed steps, resets failed steps to pending, and
/// re-enters the execution loop.
pub fn resume_workflow(input: &WorkflowResumeInput<'_>) -> Result<WorkflowResult> {
    let config = input.config;
    let db = input.db_path.clone().unwrap_or_else(crate::config::db_path);
    let raw_conn = crate::db::open_database(&db)?;
    let shared_conn = Arc::new(std::sync::Mutex::new(raw_conn));

    // Pre-execution phase: validate, reset, and prepare. Lock shared_conn for the
    // duration so all mutations complete before the FlowEngine takes over.
    let (wf_run, worktree_path, repo_path, snapshot_string) = {
        let guard = lock_shared(&shared_conn)?;
        let conn: &Connection = &guard;
        let wf_mgr = WorkflowManager::new(conn);
        let wt_mgr = WorktreeManager::new(conn, config);

        // Load and validate the workflow run
        let wf_run = wf_mgr
            .get_workflow_run(input.workflow_run_id)?
            .ok_or_else(|| {
                ConductorError::Workflow(format!(
                    "Workflow run not found: {}",
                    input.workflow_run_id
                ))
            })?;

        validate_resume_preconditions(&wf_run.status, input.restart, input.from_step)?;

        // Load steps for --from-step validation and skip-count logging.
        // Note: FlowEngine::resume() issues a second get_steps() query after all
        // DB resets complete, so it reads the accurate post-reset state.
        let all_steps = wf_mgr.get_workflow_steps(&wf_run.id)?;

        // Validate --from-step early (fail-fast before heavier worktree/snapshot operations)
        if let Some(from_step) = input.from_step {
            if !input.restart && !all_steps.iter().any(|s| s.step_name == from_step) {
                return Err(ConductorError::Workflow(format!(
                    "Step '{}' not found in workflow run '{}'",
                    from_step, wf_run.id
                )));
            }
        }

        // Fail early for ephemeral PR runs (no worktree_id, repo_id, or ticket_id).
        if wf_run.worktree_id.is_none() && wf_run.repo_id.is_none() && wf_run.ticket_id.is_none() {
            return Err(ConductorError::Workflow(format!(
            "Workflow run '{}' was an ephemeral PR run with no registered worktree — cannot resume.",
            wf_run.id
        )));
        }

        // Deserialize definition from snapshot
        let snapshot_string = wf_run
            .definition_snapshot
            .as_deref()
            .ok_or_else(|| {
                ConductorError::Workflow(format!(
                    "Workflow run '{}' has no definition snapshot — cannot resume.",
                    wf_run.id
                ))
            })?
            .to_string();

        // Determine execution paths based on target type.
        // - Worktree run: look up worktree and derive repo from it.
        // - Repo/ticket run: look up repo directly (via repo_id or ticket.repo_id).
        let (worktree_path, _worktree_slug, repo_path) = if let Some(wt_id) =
            wf_run.worktree_id.as_deref()
        {
            let worktree = wt_mgr.get_by_id(wt_id)?;
            let repo = crate::repo::RepoManager::new(conn, config).get_by_id(&worktree.repo_id)?;
            if std::path::Path::new(&worktree.path).exists() {
                (
                    worktree.path.clone(),
                    worktree.slug.clone(),
                    repo.local_path.clone(),
                )
            } else {
                tracing::warn!(
                    "Worktree path '{}' does not exist; falling back to repo root '{}'",
                    worktree.path,
                    repo.local_path
                );
                (
                    repo.local_path.clone(),
                    String::new(),
                    repo.local_path.clone(),
                )
            }
        } else {
            // Resolve repo_id from the run or via the linked ticket.
            // (The ephemeral guard above ensures at least one FK is set.)
            let effective_repo_id = if let Some(rid) = wf_run.repo_id.as_deref() {
                rid.to_string()
            } else {
                let tid = wf_run.ticket_id.as_deref().expect("ticket_id is Some when worktree_id and repo_id are both None — enforced by the ephemeral run guard above");
                crate::tickets::TicketSyncer::new(conn)
                    .get_by_id(tid)
                    .map_err(|e| {
                        ConductorError::Workflow(format!(
                            "Cannot resolve repo for ticket '{}' during resume: {e}",
                            tid
                        ))
                    })?
                    .repo_id
            };
            let repo = crate::repo::RepoManager::new(conn, config).get_by_id(&effective_repo_id)?;
            let path = repo.local_path.clone();
            (path.clone(), String::new(), path)
        };

        // Warn if any running steps have live subprocesses — terminate_subprocesses
        // (called inside reset_failed_steps below) will kill them, but the warning
        // helps diagnose concurrent executor races (see issue #2221).
        let live_count = wf_mgr.count_live_subprocess_steps(&wf_run.id)?;
        if live_count > 0 {
            tracing::warn!(
                run_id = %wf_run.id,
                live_count,
                "resume_workflow: {live_count} running step(s) have live subprocesses — \
                 terminating before reset"
            );
        }

        // Remove orphaned pending steps (registered but never started) before building the
        // skip set. These rows carry no useful state and would otherwise pollute step history.
        wf_mgr.delete_orphaned_pending_steps(&wf_run.id)?;

        // Perform DB resets and count how many completed steps will be skipped (for logging).
        let skip_count: usize = if input.restart {
            // Restart: clear all step results — skip nothing
            wf_mgr.reset_failed_steps(&wf_run.id)?;
            wf_mgr.reset_completed_steps(&wf_run.id)?;
            0
        } else {
            let completed_count = all_steps
                .iter()
                .filter(|s| s.status == WorkflowStepStatus::Completed)
                .count();

            let skip_count = if let Some(from_step) = input.from_step {
                let pos = all_steps
                    .iter()
                    .find(|s| s.step_name == from_step)
                    .ok_or_else(|| {
                        ConductorError::Workflow(format!(
                            "resume step '{}' not found in run '{}'",
                            from_step, wf_run.id
                        ))
                    })?
                    .position;

                let reset_count = all_steps
                    .iter()
                    .filter(|s| s.position >= pos && s.status == WorkflowStepStatus::Completed)
                    .count();
                // Reset those steps in DB
                wf_mgr.reset_steps_from_position(&wf_run.id, pos)?;
                completed_count - reset_count
            } else {
                completed_count
            };

            // Reset non-completed steps
            wf_mgr.reset_failed_steps(&wf_run.id)?;
            skip_count
        };

        // Reset run status to Running
        wf_mgr.update_workflow_status(&wf_run.id, WorkflowRunStatus::Running, None, None)?;

        tracing::info!(
            "Resuming workflow '{}' (run {}), {} completed steps to skip",
            wf_run.workflow_name,
            wf_run.id,
            skip_count,
        );

        (wf_run, worktree_path, repo_path, snapshot_string)
    };

    // -----------------------------------------------------------------------
    // Build the FlowEngine and execution state.
    // -----------------------------------------------------------------------

    let rk_def = deserialize_workflow_snapshot(&snapshot_string)?;

    let RkEngineComponents {
        persistence,
        action_registry,
        child_runner,
    } = build_rk_engine_components(config, &shared_conn, &db);

    let item_registry = Arc::new(super::runkon_bridge::build_rk_item_provider_registry(
        Arc::clone(&shared_conn),
        config,
        wf_run.repo_id.clone(),
    ));

    let script_env_provider =
        super::runkon_bridge::build_rk_script_env_provider(input.conductor_bin_dir.clone(), vec![]);

    let schema_resolver = make_schema_resolver(wf_run.workflow_name.clone());

    let event_sinks: Arc<[Arc<dyn runkon_flow::EventSink>]> = Arc::from(input.event_sinks.clone());

    let mut rk_state = build_rk_execution_state(RkStateArgs {
        persistence: Arc::clone(&persistence),
        action_registry,
        script_env_provider,
        workflow_run_id: wf_run.id.clone(),
        workflow_name: wf_run.workflow_name.clone(),
        worktree_ctx: runkon_flow::engine::WorktreeContext {
            worktree_id: wf_run.worktree_id.clone(),
            working_dir: worktree_path.clone(),
            repo_path,
            ticket_id: wf_run.ticket_id.clone(),
            repo_id: wf_run.repo_id.clone(),
            extra_plugin_dirs: vec![],
        },
        model: input.model.map(String::from),
        exec_config: runkon_flow::types::WorkflowExecConfig {
            shutdown: input.shutdown.clone(),
            ..runkon_flow::types::WorkflowExecConfig::default()
        },
        inputs: wf_run.inputs.clone(),
        parent_run_id: wf_run.parent_run_id.clone(),
        depth: 0,
        target_label: wf_run.target_label.clone(),
        default_bot_name: wf_run.default_bot_name.clone(),
        triggered_by_hook: wf_run.is_triggered_by_hook(),
        schema_resolver,
        child_runner,
        registry: item_registry,
        event_sinks: Arc::clone(&event_sinks),
    });

    let engine = build_flow_engine(
        &persistence,
        &event_sinks,
        worktree_path,
        wf_run.default_bot_name.clone(),
        config.clone(),
        &db,
        &wf_run.workflow_name,
    )?;

    let rk_result = engine
        .resume(&rk_def, &mut rk_state)
        .map_err(|e| map_engine_error(e, &wf_run.workflow_name, "resume"))?;

    Ok(rk_result.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::manager::WorkflowManager;
    use runkon_flow::dsl::{InputDecl, InputType, WorkflowDef, WorkflowTrigger};
    use std::collections::HashMap;

    // -------------------------------------------------------------------------
    // apply_workflow_input_defaults
    // -------------------------------------------------------------------------

    fn make_wf(inputs: Vec<InputDecl>) -> WorkflowDef {
        WorkflowDef {
            name: "test-wf".to_string(),
            title: None,
            description: String::new(),
            trigger: WorkflowTrigger::Manual,
            targets: vec![],
            group: None,
            inputs,
            body: vec![],
            always: vec![],
            source_path: String::new(),
        }
    }

    fn input_decl(
        name: &str,
        required: bool,
        input_type: InputType,
        default: Option<&str>,
    ) -> InputDecl {
        InputDecl {
            name: name.to_string(),
            required,
            input_type,
            default: default.map(str::to_string),
            description: None,
        }
    }

    #[test]
    fn apply_defaults_missing_required_returns_error() {
        let wf = make_wf(vec![input_decl("ticket", true, InputType::String, None)]);
        let mut inputs = HashMap::new();
        let err = apply_workflow_input_defaults(&wf, &mut inputs).unwrap_err();
        assert!(
            err.to_string().contains("Missing required input: 'ticket'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn apply_defaults_present_required_ok() {
        let wf = make_wf(vec![input_decl("ticket", true, InputType::String, None)]);
        let mut inputs = HashMap::from([("ticket".to_string(), "PROJ-1".to_string())]);
        apply_workflow_input_defaults(&wf, &mut inputs).expect("should succeed");
    }

    #[test]
    fn apply_defaults_inserts_default_when_key_absent() {
        let wf = make_wf(vec![input_decl(
            "env",
            false,
            InputType::String,
            Some("staging"),
        )]);
        let mut inputs = HashMap::new();
        apply_workflow_input_defaults(&wf, &mut inputs).expect("should succeed");
        assert_eq!(inputs.get("env").map(String::as_str), Some("staging"));
    }

    #[test]
    fn apply_defaults_does_not_override_existing_value() {
        let wf = make_wf(vec![input_decl(
            "env",
            false,
            InputType::String,
            Some("staging"),
        )]);
        let mut inputs = HashMap::from([("env".to_string(), "production".to_string())]);
        apply_workflow_input_defaults(&wf, &mut inputs).expect("should succeed");
        assert_eq!(inputs.get("env").map(String::as_str), Some("production"));
    }

    #[test]
    fn apply_defaults_boolean_input_defaults_to_false() {
        let wf = make_wf(vec![input_decl("dry_run", false, InputType::Boolean, None)]);
        let mut inputs = HashMap::new();
        apply_workflow_input_defaults(&wf, &mut inputs).expect("should succeed");
        assert_eq!(inputs.get("dry_run").map(String::as_str), Some("false"));
    }

    #[test]
    fn apply_defaults_required_with_default_absent_returns_error() {
        // required=true takes priority over default — absent required inputs are always an error.
        let wf = make_wf(vec![input_decl(
            "ticket",
            true,
            InputType::String,
            Some("fallback"),
        )]);
        let mut inputs = HashMap::new();
        let err = apply_workflow_input_defaults(&wf, &mut inputs).unwrap_err();
        assert!(
            err.to_string().contains("Missing required input: 'ticket'"),
            "unexpected error: {err}"
        );
    }

    // -------------------------------------------------------------------------
    // validate_resume_preconditions
    // -------------------------------------------------------------------------

    #[test]
    fn validate_resume_completed_without_restart_errors() {
        let err =
            validate_resume_preconditions(&WorkflowRunStatus::Completed, false, None).unwrap_err();
        assert!(
            err.to_string().contains("Cannot resume a completed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_resume_completed_with_restart_ok() {
        validate_resume_preconditions(&WorkflowRunStatus::Completed, true, None)
            .expect("restart on completed should be allowed");
    }

    #[test]
    fn validate_resume_running_errors() {
        let err =
            validate_resume_preconditions(&WorkflowRunStatus::Running, false, None).unwrap_err();
        assert!(
            err.to_string().contains("already running"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_resume_cancelled_errors() {
        let err =
            validate_resume_preconditions(&WorkflowRunStatus::Cancelled, false, None).unwrap_err();
        assert!(
            err.to_string().contains("Cannot resume a cancelled"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_resume_restart_with_from_step_errors() {
        let err = validate_resume_preconditions(&WorkflowRunStatus::Failed, true, Some("step-2"))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Cannot use --restart and --from-step"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_resume_failed_without_restart_ok() {
        validate_resume_preconditions(&WorkflowRunStatus::Failed, false, None)
            .expect("resuming a failed run should be allowed");
    }

    #[test]
    fn validate_resume_failed_with_restart_ok() {
        validate_resume_preconditions(&WorkflowRunStatus::Failed, true, None)
            .expect("restarting a failed run should be allowed");
    }

    #[test]
    fn validate_resume_needs_resume_ok() {
        // NeedsResume falls through to Ok(()) — it is an explicitly resumable status.
        validate_resume_preconditions(&WorkflowRunStatus::NeedsResume, false, None)
            .expect("NeedsResume should be resumable");
    }

    #[test]
    fn validate_resume_cancelling_ok() {
        // Cancelling falls through to Ok(()) — partial cancellation can be resumed.
        validate_resume_preconditions(&WorkflowRunStatus::Cancelling, false, None)
            .expect("Cancelling should be resumable");
    }

    #[test]
    fn validate_resume_waiting_ok() {
        // Waiting falls through to Ok(()) — it is an explicitly resumable status.
        validate_resume_preconditions(&WorkflowRunStatus::Waiting, false, None)
            .expect("Waiting should be resumable");
    }

    // -------------------------------------------------------------------------
    // spawn_heartbeat_resume
    // -------------------------------------------------------------------------

    #[test]
    fn spawn_heartbeat_resume_failure_with_valid_db_fires_notification_without_panic() {
        // Branch 1: resume fails (invalid run_id), DB opens successfully → notification fires.
        use tempfile::NamedTempFile;
        let db_file = NamedTempFile::new().unwrap();
        let db_path = db_file.path().to_path_buf();
        // Initialise the schema so open_database succeeds inside the spawned thread.
        crate::db::open_database(&db_path).unwrap();

        let handle = spawn_heartbeat_resume(SpawnHeartbeatResumeParams {
            run_id: "nonexistent-run-id".to_string(),
            workflow_name: "test-wf".to_string(),
            target_label: None,
            config: crate::config::Config::default(),
            conductor_bin_dir: None,
            db_path: Some(db_path),
        });
        handle
            .join()
            .expect("spawn_heartbeat_resume thread panicked");
    }

    #[test]
    fn spawn_heartbeat_resume_failure_with_invalid_db_logs_warning_without_panic() {
        // Branch 2: resume fails, DB open also fails (non-existent dir) → warning logged, no panic.
        let handle = spawn_heartbeat_resume(SpawnHeartbeatResumeParams {
            run_id: "nonexistent-run-id".to_string(),
            workflow_name: "test-wf".to_string(),
            target_label: None,
            config: crate::config::Config::default(),
            conductor_bin_dir: None,
            db_path: Some(std::path::PathBuf::from(
                "/nonexistent/path/that/cannot/be/opened/conductor.db",
            )),
        });
        handle
            .join()
            .expect("spawn_heartbeat_resume thread panicked");
    }

    // -------------------------------------------------------------------------
    // spawn_workflow_resume
    // -------------------------------------------------------------------------

    #[test]
    fn spawn_workflow_resume_does_not_panic_on_invalid_run_id() {
        // resume_workflow_standalone fails (run not found) → warning is logged, no panic.
        let handle = spawn_workflow_resume(
            "nonexistent-run-id".to_string(),
            crate::config::Config::default(),
            None,
        );
        handle
            .join()
            .expect("spawn_workflow_resume thread panicked");
    }

    // -------------------------------------------------------------------------
    // guard_active_run
    // -------------------------------------------------------------------------

    fn setup_guard_db() -> rusqlite::Connection {
        crate::test_helpers::setup_db()
    }

    fn make_running_workflow_run(
        conn: &rusqlite::Connection,
        wt_id: &str,
        wf_name: &str,
    ) -> String {
        let agent_mgr = crate::agent::AgentManager::new(conn);
        let parent = agent_mgr.create_run(Some(wt_id), "workflow", None).unwrap();
        let wf_mgr = WorkflowManager::new(conn);
        let run = wf_mgr
            .create_workflow_run(wf_name, Some(wt_id), &parent.id, false, "manual", None)
            .unwrap();
        // Transition to Running so it counts as an active run.
        conn.execute(
            "UPDATE workflow_runs SET status = 'running' WHERE id = ?1",
            rusqlite::params![run.id],
        )
        .unwrap();
        run.id
    }

    #[test]
    fn guard_active_run_returns_already_active_when_run_exists() {
        let conn = setup_guard_db();
        let wf_name = "my-workflow";
        make_running_workflow_run(&conn, "w1", wf_name);

        let wf_mgr = WorkflowManager::new(&conn);
        let err = guard_active_run(&wf_mgr, "w1", false).unwrap_err();
        assert!(
            matches!(err, crate::error::ConductorError::WorkflowRunAlreadyActive { ref name } if name == wf_name),
            "expected WorkflowRunAlreadyActive, got: {err}"
        );
    }

    #[test]
    fn guard_active_run_ok_when_no_active_run() {
        let conn = setup_guard_db();
        let wf_mgr = WorkflowManager::new(&conn);
        // No workflow runs exist — guard should pass.
        guard_active_run(&wf_mgr, "w1", false).expect("no active run should return Ok");
    }

    #[test]
    fn guard_active_run_force_cancels_active_run() {
        let conn = setup_guard_db();
        let run_id = make_running_workflow_run(&conn, "w1", "my-wf");

        let wf_mgr = WorkflowManager::new(&conn);
        guard_active_run(&wf_mgr, "w1", true).expect("force should cancel and return Ok");

        // The previously active run must now be cancelled.
        let row: String = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            row, "cancelled",
            "active run should be cancelled after force override"
        );
    }

    #[test]
    fn guard_active_run_ok_when_only_completed_runs_exist() {
        let conn = setup_guard_db();
        let agent_mgr = crate::agent::AgentManager::new(&conn);
        let parent = agent_mgr.create_run(Some("w1"), "workflow", None).unwrap();
        let wf_mgr = WorkflowManager::new(&conn);
        let run = wf_mgr
            .create_workflow_run("wf", Some("w1"), &parent.id, false, "manual", None)
            .unwrap();
        conn.execute(
            "UPDATE workflow_runs SET status = 'completed' WHERE id = ?1",
            rusqlite::params![run.id],
        )
        .unwrap();

        // Completed run is not "active" — guard should pass.
        guard_active_run(&wf_mgr, "w1", false).expect("completed run should not block new run");
    }

    // -------------------------------------------------------------------------
    // inject_ticket_variables / inject_worktree_variables / inject_repo_variables
    // -------------------------------------------------------------------------

    fn make_ticket() -> crate::tickets::Ticket {
        crate::tickets::Ticket {
            id: "t1".into(),
            source_id: "42".into(),
            source_type: "github".into(),
            title: "Fix bug".into(),
            body: "body text".into(),
            state: "open".into(),
            url: "https://github.com/org/repo/issues/42".into(),
            priority: None,
            repo_id: "r1".into(),
            labels: String::new(),
            raw_json: "{}".into(),
            synced_at: "2024-01-01T00:00:00Z".into(),
            assignee: None,
            workflow: None,
            agent_map: None,
        }
    }

    #[test]
    fn inject_ticket_variables_populates_all_keys() {
        let ticket = make_ticket();
        let mut inputs = HashMap::new();
        inject_ticket_variables(&ticket, &mut inputs);

        assert_eq!(inputs["ticket_id"], "t1");
        assert_eq!(inputs["ticket_source_id"], "42");
        assert_eq!(inputs["ticket_source_type"], "github");
        assert_eq!(inputs["ticket_title"], "Fix bug");
        assert_eq!(inputs["ticket_body"], "body text");
        assert_eq!(
            inputs["ticket_url"],
            "https://github.com/org/repo/issues/42"
        );
        assert_eq!(inputs["ticket_raw_json"], "{}");
    }

    #[test]
    fn inject_ticket_variables_does_not_overwrite_existing_keys() {
        let ticket = make_ticket();
        let mut inputs = HashMap::new();
        inputs.insert("ticket_title".into(), "caller-supplied".into());
        inject_ticket_variables(&ticket, &mut inputs);
        assert_eq!(
            inputs["ticket_title"], "caller-supplied",
            "pre-existing key must not be overwritten"
        );
    }

    #[test]
    fn inject_worktree_variables_populates_branch_and_base() {
        let conn = crate::test_helpers::setup_db();
        let config = crate::config::Config::default();
        let wt_mgr = crate::worktree::WorktreeManager::new(&conn, &config);
        let wt = wt_mgr.get_by_id("w1").unwrap();
        let mut inputs = HashMap::new();
        inject_worktree_variables(&wt, "main", &mut inputs);
        assert!(
            inputs.contains_key("worktree_branch"),
            "worktree_branch must be injected"
        );
        assert!(
            inputs.contains_key("feature_base_branch"),
            "feature_base_branch must be injected"
        );
    }

    // -------------------------------------------------------------------------
    // execute_workflow_standalone — error paths and run_id_notify
    // -------------------------------------------------------------------------

    fn make_standalone_params(
        db_path: std::path::PathBuf,
        ticket_id: Option<String>,
        run_id_notify: Option<crate::workflow::types::RunIdSlot>,
    ) -> WorkflowExecStandalone {
        WorkflowExecStandalone {
            config: crate::config::Config::default(),
            workflow: make_wf(vec![]),
            worktree_id: if ticket_id.is_some() {
                Some("w1".into())
            } else {
                None
            },
            working_dir: "/tmp".into(),
            repo_path: "/tmp".into(),
            ticket_id,
            repo_id: None,
            model: None,
            exec_config: crate::workflow::types::WorkflowExecConfig {
                dry_run: false,
                ..Default::default()
            },
            inputs: HashMap::new(),
            target_label: None,
            run_id_notify,
            triggered_by_hook: false,
            conductor_bin_dir: None,
            force: false,
            extra_plugin_dirs: vec![],
            db_path: Some(db_path),
            parent_workflow_run_id: None,
            depth: 0,
            parent_step_id: None,
            default_bot_name: None,
            iteration: 0,
        }
    }

    #[test]
    fn execute_standalone_unknown_ticket_id_returns_error() {
        // workflow_runs.ticket_id has a FK constraint — providing a nonexistent ticket
        // causes the run-creation INSERT to fail, which is surfaced as an error from
        // execute_workflow_standalone before any agent work begins.
        let db_file = tempfile::NamedTempFile::new().unwrap();
        {
            let conn = crate::db::open_database(db_file.path()).unwrap();
            crate::test_helpers::insert_test_repo(&conn, "r1", "test-repo", "/tmp/repo");
            crate::test_helpers::insert_test_worktree(&conn, "w1", "r1", "feat-test", "/tmp");
        }

        let params = make_standalone_params(
            db_file.path().to_path_buf(),
            Some("no-such-ticket".into()),
            None,
        );
        let result = execute_workflow_standalone(&params);
        assert!(
            result.is_err(),
            "nonexistent ticket_id must produce an error"
        );
    }

    // -------------------------------------------------------------------------
    // resume_workflow — from_step not-found and ephemeral PR run
    // -------------------------------------------------------------------------

    fn make_resume_input<'a>(
        config: &'a crate::config::Config,
        run_id: &'a str,
        from_step: Option<&'a str>,
        db_path: std::path::PathBuf,
    ) -> WorkflowResumeInput<'a> {
        WorkflowResumeInput {
            config,
            workflow_run_id: run_id,
            model: None,
            from_step,
            restart: false,
            conductor_bin_dir: None,
            event_sinks: vec![],
            db_path: Some(db_path),
            shutdown: None,
        }
    }

    #[test]
    fn resume_workflow_from_step_not_found_returns_err() {
        use tempfile::NamedTempFile;
        let db_file = NamedTempFile::new().unwrap();
        let run_id = {
            let conn = crate::db::open_database(db_file.path()).unwrap();
            crate::test_helpers::insert_test_repo(&conn, "r1", "test-repo", "/tmp/repo");
            crate::test_helpers::insert_test_worktree(&conn, "w1", "r1", "feat-test", "/tmp");
            let parent = crate::agent::AgentManager::new(&conn)
                .create_run(Some("w1"), "workflow", None)
                .unwrap();
            let wf_mgr = WorkflowManager::new(&conn);
            let run = wf_mgr
                .create_workflow_run("test-wf", Some("w1"), &parent.id, false, "manual", None)
                .unwrap();
            conn.execute(
                "UPDATE workflow_runs SET status = 'failed' WHERE id = ?1",
                rusqlite::params![run.id],
            )
            .unwrap();
            run.id
        };

        let config = crate::config::Config::default();
        let input = make_resume_input(
            &config,
            &run_id,
            Some("no-such-step"),
            db_file.path().to_path_buf(),
        );
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "expected 'not found' error, got: {err}"
        );
    }

    #[test]
    fn resume_workflow_ephemeral_pr_run_returns_err() {
        use tempfile::NamedTempFile;
        let db_file = NamedTempFile::new().unwrap();
        let run_id = {
            let conn = crate::db::open_database(db_file.path()).unwrap();
            // Ephemeral runs have no worktree/repo/ticket — only an agent parent.
            let parent = crate::agent::AgentManager::new(&conn)
                .create_run(None, "workflow", None)
                .unwrap();
            let wf_mgr = WorkflowManager::new(&conn);
            let run = wf_mgr
                .create_workflow_run_with_targets(
                    "test-wf", None, // worktree_id
                    None, // ticket_id
                    None, // repo_id
                    &parent.id, false, "manual", None, None, None,
                )
                .unwrap();
            conn.execute(
                "UPDATE workflow_runs SET status = 'failed' WHERE id = ?1",
                rusqlite::params![run.id],
            )
            .unwrap();
            run.id
        };

        let config = crate::config::Config::default();
        let input = make_resume_input(&config, &run_id, None, db_file.path().to_path_buf());
        let err = resume_workflow(&input).unwrap_err();
        assert!(
            err.to_string().contains("ephemeral"),
            "expected 'ephemeral' error, got: {err}"
        );
    }

    #[test]
    fn execute_standalone_run_id_notify_populated() {
        // With no worktree/ticket/repo IDs, the function runs an empty workflow trivially
        // (no steps → immediate success). This documents that run_id_notify is populated
        // immediately after the workflow run record is created, before any engine work.
        let db_file = tempfile::NamedTempFile::new().unwrap();
        crate::db::open_database(db_file.path()).unwrap();

        let notify: crate::workflow::types::RunIdSlot =
            std::sync::Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));
        let params = make_standalone_params(
            db_file.path().to_path_buf(),
            None,
            Some(std::sync::Arc::clone(&notify)),
        );
        let _ = execute_workflow_standalone(&params);

        let (lock, _) = notify.as_ref();
        let slot = lock.lock().unwrap();
        assert!(
            slot.is_some(),
            "run_id_notify slot must be populated before any engine work"
        );
    }
}
