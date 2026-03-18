use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::agent::AgentManager;
use crate::agent_config::AgentSpec;
use crate::config::Config;
use crate::error::{ConductorError, Result};
use crate::schema_config::{OutputSchema, SchemaIssue};
use crate::workflow_dsl::{self, WorkflowDef, WorkflowNode};
use crate::worktree::WorktreeManager;

use super::manager::WorkflowManager;
use super::status::{WorkflowRunStatus, WorkflowStepStatus};
use super::types::{
    ContextEntry, StepKey, StepResult, WorkflowExecConfig, WorkflowExecInput,
    WorkflowExecStandalone, WorkflowResult, WorkflowResumeInput, WorkflowResumeStandalone,
    WorkflowRunStep,
};

/// Input keys that the workflow engine injects automatically from the run context
/// (ticket and repo metadata). Consumers can use this slice to identify inputs
/// that are read-only from the user's perspective.
pub const ENGINE_INJECTED_KEYS: &[&str] = &[
    "ticket_id",
    "ticket_source_id",
    "ticket_title",
    "ticket_url",
    "repo_id",
    "repo_path",
    "repo_name",
    "feature_id",
    "feature_name",
    "feature_branch",
];

/// Pre-loaded context for resuming a workflow run.
///
/// Separated from [`ExecutionState`] so that fresh runs carry no resume
/// overhead and the borrow-splitting between "read completed data" and
/// "mutate execution state" is explicit.
pub(super) struct ResumeContext {
    /// Step keys to skip (e.g. `("lint", 0)`).
    pub skip_completed: HashSet<StepKey>,
    /// Completed step records keyed by step key, for O(1) restore.
    pub step_map: HashMap<StepKey, WorkflowRunStep>,
    /// Pre-loaded child agent runs keyed by run ID, avoiding N+1 queries
    /// when accumulating costs during restore.
    pub child_runs: HashMap<String, crate::agent::AgentRun>,
}

/// Mutable runtime state for a workflow execution.
pub(super) struct ExecutionState<'a> {
    pub conn: &'a Connection,
    pub config: &'a Config,
    pub workflow_run_id: String,
    pub workflow_name: String,
    pub worktree_id: Option<String>,
    pub working_dir: String,
    pub worktree_slug: String,
    pub repo_path: String,
    pub ticket_id: Option<String>,
    pub repo_id: Option<String>,
    pub model: Option<String>,
    pub exec_config: WorkflowExecConfig,
    pub inputs: HashMap<String, String>,
    pub agent_mgr: AgentManager<'a>,
    pub wf_mgr: WorkflowManager<'a>,
    pub parent_run_id: String,
    /// Current nesting depth (0 = top-level workflow).
    pub depth: u32,
    /// Human-readable label for the target (inherited by sub-workflows).
    pub target_label: Option<String>,
    // Runtime
    pub step_results: HashMap<String, StepResult>,
    pub contexts: Vec<ContextEntry>,
    pub position: i64,
    pub all_succeeded: bool,
    pub total_cost: f64,
    pub total_turns: i64,
    pub total_duration_ms: i64,
    pub last_gate_feedback: Option<String>,
    /// Block-level output schema name inherited from an enclosing `do {}` block.
    pub block_output: Option<String>,
    /// Block-level prompt snippet refs inherited from an enclosing `do {}` block.
    pub block_with: Vec<String>,
    /// Resume context — `None` for fresh runs, `Some` when resuming.
    pub resume_ctx: Option<ResumeContext>,
    /// Default named GitHub App bot identity inherited from a parent `call workflow { as = "..." }`.
    pub default_bot_name: Option<String>,
    /// Optional feature ID linking this run to a feature branch.
    pub feature_id: Option<String>,
}

/// Resolve a schema by name using the standard search order.
pub(super) fn resolve_schema(state: &ExecutionState<'_>, name: &str) -> Result<OutputSchema> {
    let schema_ref = crate::schema_config::SchemaRef::from_str_value(name);
    crate::schema_config::load_schema(
        &state.working_dir,
        &state.repo_path,
        &schema_ref,
        Some(&state.workflow_name),
    )
}

/// Extract completed step keys from a slice of step records.
///
/// Shared by [`WorkflowManager::get_completed_step_keys`] and [`resume_workflow`]
/// so the key-building logic lives in one place.
pub(super) fn completed_keys_from_steps(steps: &[WorkflowRunStep]) -> HashSet<StepKey> {
    steps
        .iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .map(|s| (s.step_name.clone(), s.iteration as u32))
        .collect()
}

/// Validate required workflow inputs are present and apply default values.
///
/// Returns an error if a required input is missing.
pub fn apply_workflow_input_defaults(
    workflow: &WorkflowDef,
    inputs: &mut HashMap<String, String>,
) -> Result<()> {
    use crate::workflow_dsl::InputType;
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

/// Execute a workflow definition against a worktree.
pub fn execute_workflow(input: &WorkflowExecInput<'_>) -> Result<WorkflowResult> {
    let conn = input.conn;
    let config = input.config;
    let workflow = input.workflow;

    let agent_mgr = AgentManager::new(conn);
    let wf_mgr = WorkflowManager::new(conn);
    let worktree_slug = if let Some(wt_id) = input.worktree_id {
        let wt_mgr = WorktreeManager::new(conn, config);
        let wt = wt_mgr.get_by_id(wt_id)?;
        if std::path::Path::new(&wt.path).exists() {
            wt.slug
        } else {
            tracing::warn!(
                "Worktree path '{}' does not exist; falling back to repo root for slug",
                wt.path
            );
            String::new()
        }
    } else {
        String::new()
    };

    // Validate all referenced agents exist before starting
    let mut all_agents = workflow_dsl::collect_agent_names(&workflow.body);
    all_agents.extend(workflow_dsl::collect_agent_names(&workflow.always));
    all_agents.sort();
    all_agents.dedup();

    let specs: Vec<AgentSpec> = all_agents.iter().map(AgentSpec::from).collect();
    let missing_agents = crate::agent_config::find_missing_agents(
        input.working_dir,
        input.repo_path,
        &specs,
        Some(&workflow.name),
    );
    if !missing_agents.is_empty() {
        return Err(ConductorError::Workflow(format!(
            "Missing agent definitions: {}. Run 'conductor workflow validate' for details.",
            missing_agents.join(", ")
        )));
    }

    // Validate all referenced prompt snippets exist before starting
    let all_snippets = workflow.collect_all_snippet_refs();

    if !all_snippets.is_empty() {
        let missing_snippets = crate::prompt_config::find_missing_snippets(
            input.working_dir,
            input.repo_path,
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

    // Validate all referenced output schemas exist and parse correctly
    let all_schemas = workflow.collect_all_schema_refs();
    if !all_schemas.is_empty() {
        let schema_issues = crate::schema_config::check_schemas(
            input.working_dir,
            input.repo_path,
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

    // Snapshot the definition
    let snapshot_json = serde_json::to_string(workflow).map_err(|e| {
        ConductorError::Workflow(format!("Failed to serialize workflow definition: {e}"))
    })?;

    // Guard: prevent multiple concurrent top-level runs on the same worktree
    // (skipped for ephemeral PR runs which have no registered worktree).
    if input.depth == 0 {
        if let Some(wt_id) = input.worktree_id {
            if let Some(active) = wf_mgr.get_active_run_for_worktree(wt_id)? {
                return Err(ConductorError::WorkflowRunAlreadyActive {
                    name: active.workflow_name,
                });
            }
        }
    }

    // Create parent agent run (uses empty worktree_id for ephemeral PR runs).
    let parent_prompt = format!("Workflow: {} — {}", workflow.name, workflow.description);
    let parent_run = agent_mgr.create_run(input.worktree_id, &parent_prompt, None, input.model)?;

    // Create workflow run record with snapshot and target FKs in a single INSERT
    let wf_run = wf_mgr.create_workflow_run_with_targets(
        &workflow.name,
        input.worktree_id,
        input.ticket_id,
        input.repo_id,
        &parent_run.id,
        input.exec_config.dry_run,
        &workflow.trigger.to_string(),
        Some(&snapshot_json),
        input.parent_workflow_run_id,
        input.target_label,
    )?;

    // Notify any waiting caller of the freshly-created run ID.
    if let Some(pair) = &input.run_id_notify {
        let (lock, cvar) = pair.as_ref();
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = Some(wf_run.id.clone());
        cvar.notify_one();
    }

    // Persist feature_id on the workflow run record.
    if let Some(fid) = input.feature_id {
        wf_mgr.set_workflow_run_feature_id(&wf_run.id, fid)?;
    }

    // Persist default_bot_name so it can be restored on resume.
    if let Some(ref bot_name) = input.default_bot_name {
        wf_mgr.set_workflow_run_default_bot_name(&wf_run.id, bot_name)?;
    }

    // Persist loop iteration number for sub-workflow runs.
    if input.iteration > 0 {
        wf_mgr.set_workflow_run_iteration(&wf_run.id, input.iteration as i64)?;
    }

    // Build inputs map, injecting implicit ticket/repo variables
    let mut merged_inputs = input.inputs.clone();
    if let Some(tid) = input.ticket_id {
        let ticket = crate::tickets::TicketSyncer::new(conn).get_by_id(tid)?;
        merged_inputs
            .entry("ticket_id".to_string())
            .or_insert_with(|| ticket.id.clone());
        merged_inputs
            .entry("ticket_source_id".to_string())
            .or_insert_with(|| ticket.source_id.clone());
        merged_inputs
            .entry("ticket_title".to_string())
            .or_insert_with(|| ticket.title.clone());
        merged_inputs
            .entry("ticket_url".to_string())
            .or_insert_with(|| ticket.url.clone());
    }
    if let Some(rid) = input.repo_id {
        let repo = crate::repo::RepoManager::new(conn, config).get_by_id(rid)?;
        merged_inputs
            .entry("repo_id".to_string())
            .or_insert_with(|| repo.id.clone());
        merged_inputs
            .entry("repo_path".to_string())
            .or_insert_with(|| repo.local_path.clone());
        merged_inputs
            .entry("repo_name".to_string())
            .or_insert_with(|| repo.slug.clone());
    }

    // Inject feature metadata when a feature_id is provided.
    if let Some(fid) = input.feature_id {
        let feature = conn
            .query_row(
                "SELECT id, name, branch FROM features WHERE id = ?1",
                rusqlite::params![fid],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    ConductorError::Workflow(format!("Feature not found: {fid}"))
                }
                _ => ConductorError::Database(e),
            })?;
        merged_inputs
            .entry("feature_id".to_string())
            .or_insert_with(|| feature.0);
        merged_inputs
            .entry("feature_name".to_string())
            .or_insert_with(|| feature.1);
        merged_inputs
            .entry("feature_branch".to_string())
            .or_insert_with(|| feature.2);
    }

    // Persist inputs so they can be restored on resume
    if !merged_inputs.is_empty() {
        wf_mgr.set_workflow_run_inputs(&wf_run.id, &merged_inputs)?;
    }

    // Mark as running
    wf_mgr.update_workflow_status(&wf_run.id, WorkflowRunStatus::Running, None)?;

    let mut state = ExecutionState {
        conn,
        config,
        workflow_run_id: wf_run.id.clone(),
        workflow_name: workflow.name.clone(),
        worktree_id: input.worktree_id.map(String::from),
        working_dir: input.working_dir.to_string(),
        worktree_slug,
        repo_path: input.repo_path.to_string(),
        ticket_id: input.ticket_id.map(String::from),
        repo_id: input.repo_id.map(String::from),
        model: input.model.map(String::from),
        exec_config: input.exec_config.clone(),
        inputs: merged_inputs,
        agent_mgr: AgentManager::new(conn),
        wf_mgr: WorkflowManager::new(conn),
        parent_run_id: parent_run.id.clone(),
        depth: input.depth,
        target_label: input.target_label.map(String::from),
        step_results: HashMap::new(),
        contexts: Vec::new(),
        position: 0,
        all_succeeded: true,
        total_cost: 0.0,
        total_turns: 0,
        total_duration_ms: 0,
        last_gate_feedback: None,
        block_output: None,
        block_with: Vec::new(),
        resume_ctx: None,
        default_bot_name: input.default_bot_name.clone(),
        feature_id: input.feature_id.map(String::from),
    };

    run_workflow_engine(&mut state, workflow)
}

/// Shared orchestration: execute body → always block → build summary → finalize.
///
/// Both `execute_workflow` and `resume_workflow` delegate here after constructing
/// their `ExecutionState`.
pub(super) fn run_workflow_engine(
    state: &mut ExecutionState<'_>,
    workflow: &WorkflowDef,
) -> Result<WorkflowResult> {
    // Execute main body
    let mut body_error: Option<String> = None;
    let body_result = execute_nodes(state, &workflow.body);
    if let Err(ref e) = body_result {
        let msg = e.to_string();
        tracing::error!("Body execution error: {msg}");
        state.all_succeeded = false;
        body_error = Some(msg);
    }

    // Execute always block regardless of outcome
    if !workflow.always.is_empty() {
        let workflow_status = if state.all_succeeded {
            "completed"
        } else {
            "failed"
        };
        state
            .inputs
            .insert("workflow_status".to_string(), workflow_status.to_string());
        let always_result = execute_nodes(state, &workflow.always);
        if let Err(ref e) = always_result {
            tracing::warn!("Always block error (non-fatal): {e}");
        }
    }

    // Build summary
    let mut summary = super::helpers::build_workflow_summary(state);
    if let Some(ref err) = body_error {
        summary.push_str(&format!("\nError: {err}"));
    }

    // Finalize
    let wf_run_id = state.workflow_run_id.clone();
    let parent_run_id = state.parent_run_id.clone();
    if state.all_succeeded {
        state.agent_mgr.update_run_completed(
            &parent_run_id,
            None,
            Some(&summary),
            Some(state.total_cost),
            Some(state.total_turns),
            Some(state.total_duration_ms),
            None,
            None,
            None,
            None,
        )?;
        state.wf_mgr.update_workflow_status(
            &wf_run_id,
            WorkflowRunStatus::Completed,
            Some(&summary),
        )?;
        tracing::info!("Workflow '{}' completed successfully", workflow.name);
    } else {
        state
            .agent_mgr
            .update_run_failed(&parent_run_id, &summary)?;
        state.wf_mgr.update_workflow_status(
            &wf_run_id,
            WorkflowRunStatus::Failed,
            Some(&summary),
        )?;
        tracing::warn!("Workflow '{}' finished with failures", workflow.name);
    }

    tracing::info!(
        "Total: ${:.4}, {} turns, {:.1}s",
        state.total_cost,
        state.total_turns,
        state.total_duration_ms as f64 / 1000.0
    );

    Ok(WorkflowResult {
        workflow_run_id: wf_run_id,
        worktree_id: state.worktree_id.clone(),
        workflow_name: workflow.name.clone(),
        all_succeeded: state.all_succeeded,
        total_cost: state.total_cost,
        total_turns: state.total_turns,
        total_duration_ms: state.total_duration_ms,
    })
}

/// Execute a workflow in a self-contained manner: opens its own database
/// connection and resolves the conductor binary path. Designed for use in
/// background threads where the caller cannot share a `&Connection`.
pub fn execute_workflow_standalone(params: &WorkflowExecStandalone) -> Result<WorkflowResult> {
    let db = crate::config::db_path();
    let conn = crate::db::open_database(&db)?;

    let input = WorkflowExecInput {
        conn: &conn,
        config: &params.config,
        workflow: &params.workflow,
        worktree_id: params.worktree_id.as_deref(),
        working_dir: &params.working_dir,
        repo_path: &params.repo_path,
        ticket_id: params.ticket_id.as_deref(),
        repo_id: params.repo_id.as_deref(),
        model: params.model.as_deref(),
        exec_config: &params.exec_config,
        inputs: params.inputs.clone(),
        depth: 0,
        parent_workflow_run_id: None,
        target_label: params.target_label.as_deref(),
        default_bot_name: None,
        feature_id: params.feature_id.as_deref(),
        iteration: 0,
        run_id_notify: params.run_id_notify.clone(),
    };

    execute_workflow(&input)
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
    let conn = crate::db::open_database(&db)?;

    let input = WorkflowResumeInput {
        conn: &conn,
        config: &params.config,
        workflow_run_id: &params.workflow_run_id,
        model: params.model.as_deref(),
        from_step: params.from_step.as_deref(),
        restart: params.restart,
    };

    resume_workflow(&input)
}

/// Resume a failed or stalled workflow run from the point of failure.
///
/// Loads the workflow definition from the run's `definition_snapshot`, rebuilds
/// the skip set from completed steps, resets failed steps to pending, and
/// re-enters the execution loop.
pub fn resume_workflow(input: &WorkflowResumeInput<'_>) -> Result<WorkflowResult> {
    let conn = input.conn;
    let config = input.config;
    let wf_mgr = WorkflowManager::new(conn);
    let wt_mgr = WorktreeManager::new(conn, config);

    // Load and validate the workflow run
    let wf_run = wf_mgr
        .get_workflow_run(input.workflow_run_id)?
        .ok_or_else(|| {
            ConductorError::Workflow(format!("Workflow run not found: {}", input.workflow_run_id))
        })?;

    validate_resume_preconditions(&wf_run.status, input.restart, input.from_step)?;

    // Load all steps once (avoids N+1 queries later)
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
    let snapshot = wf_run.definition_snapshot.as_deref().ok_or_else(|| {
        ConductorError::Workflow(format!(
            "Workflow run '{}' has no definition snapshot — cannot resume.",
            wf_run.id
        ))
    })?;
    let workflow: WorkflowDef = serde_json::from_str(snapshot).map_err(|e| {
        ConductorError::Workflow(format!("Failed to deserialize workflow snapshot: {e}"))
    })?;

    // Determine execution paths based on target type.
    // - Worktree run: look up worktree and derive repo from it.
    // - Repo/ticket run: look up repo directly (via repo_id or ticket.repo_id).
    let (worktree_path, worktree_slug, repo_path) =
        if let Some(wt_id) = wf_run.worktree_id.as_deref() {
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
                let tid = wf_run.ticket_id.as_deref().expect("guarded above");
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

    // Build the skip set
    let skip_completed = if input.restart {
        // Restart: clear all step results — skip nothing
        wf_mgr.reset_failed_steps(&wf_run.id)?;
        wf_mgr.reset_completed_steps(&wf_run.id)?;
        HashSet::new()
    } else {
        let mut keys = completed_keys_from_steps(&all_steps);

        // Handle --from-step: remove completed keys at or after the specified step
        if let Some(from_step) = input.from_step {
            // Safety: from_step existence was validated above
            let pos = all_steps
                .iter()
                .find(|s| s.step_name == from_step)
                .expect("from_step validated above")
                .position;

            let to_remove: Vec<StepKey> = all_steps
                .iter()
                .filter(|s| s.position >= pos && s.status == WorkflowStepStatus::Completed)
                .map(|s| (s.step_name.clone(), s.iteration as u32))
                .collect();
            for key in to_remove {
                keys.remove(&key);
            }
            // Reset those steps in DB
            wf_mgr.reset_steps_from_position(&wf_run.id, pos)?;
        }

        // Reset non-completed steps
        wf_mgr.reset_failed_steps(&wf_run.id)?;
        keys
    };

    // Build the step map from `all_steps` (only the keys still in skip_completed
    // survived any --from-step pruning, so filter by membership).
    let step_map: HashMap<StepKey, WorkflowRunStep> = all_steps
        .into_iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .map(|s| {
            let key = (s.step_name.clone(), s.iteration as u32);
            (key, s)
        })
        .filter(|(key, _)| skip_completed.contains(key))
        .collect();

    // Batch-load child agent runs in a single query to avoid N+1 during cost accumulation
    let agent_mgr = AgentManager::new(conn);
    let child_run_ids: Vec<&str> = step_map
        .values()
        .filter_map(|s| s.child_run_id.as_deref())
        .collect();
    let child_runs = agent_mgr.get_runs_by_ids(&child_run_ids)?;

    let resume_ctx = if skip_completed.is_empty() {
        None
    } else {
        Some(ResumeContext {
            skip_completed,
            step_map,
            child_runs,
        })
    };

    // Reset run status to Running
    wf_mgr.update_workflow_status(&wf_run.id, WorkflowRunStatus::Running, None)?;

    tracing::info!(
        "Resuming workflow '{}' (run {}), {} completed steps to skip",
        workflow.name,
        wf_run.id,
        resume_ctx
            .as_ref()
            .map_or(0, |ctx| ctx.skip_completed.len()),
    );

    let mut state = ExecutionState {
        conn,
        config,
        workflow_run_id: wf_run.id.clone(),
        workflow_name: workflow.name.clone(),
        worktree_id: wf_run.worktree_id.clone(),
        working_dir: worktree_path,
        worktree_slug,
        repo_path,
        ticket_id: wf_run.ticket_id.clone(),
        repo_id: wf_run.repo_id.clone(),
        model: input.model.map(String::from),
        exec_config: WorkflowExecConfig::default(),
        inputs: wf_run.inputs.clone(),
        agent_mgr: AgentManager::new(conn),
        wf_mgr: WorkflowManager::new(conn),
        parent_run_id: wf_run.parent_run_id.clone(),
        depth: 0,
        target_label: wf_run.target_label.clone(),
        step_results: HashMap::new(),
        contexts: Vec::new(),
        position: 0,
        all_succeeded: true,
        total_cost: 0.0,
        total_turns: 0,
        total_duration_ms: 0,
        last_gate_feedback: None,
        block_output: None,
        block_with: Vec::new(),
        resume_ctx,
        default_bot_name: wf_run.default_bot_name.clone(),
        feature_id: wf_run.feature_id.clone(),
    };

    run_workflow_engine(&mut state, &workflow)
}

/// Walk a list of workflow nodes, dispatching to the appropriate handler.
pub(super) fn execute_single_node(
    state: &mut ExecutionState<'_>,
    node: &WorkflowNode,
    iteration: u32,
) -> Result<()> {
    match node {
        WorkflowNode::Call(n) => super::executors::execute_call(state, n, iteration)?,
        WorkflowNode::CallWorkflow(n) => {
            super::executors::execute_call_workflow(state, n, iteration)?
        }
        WorkflowNode::If(n) => super::executors::execute_if(state, n)?,
        WorkflowNode::Unless(n) => super::executors::execute_unless(state, n)?,
        WorkflowNode::While(n) => super::executors::execute_while(state, n)?,
        WorkflowNode::DoWhile(n) => super::executors::execute_do_while(state, n)?,
        WorkflowNode::Do(n) => super::executors::execute_do(state, n)?,
        WorkflowNode::Parallel(n) => super::executors::execute_parallel(state, n, iteration)?,
        WorkflowNode::Gate(n) => super::executors::execute_gate(state, n, iteration)?,
        WorkflowNode::Script(n) => super::executors::execute_script(state, n, iteration)?,
        WorkflowNode::Always(n) => {
            // Nested always — just execute body
            execute_nodes(state, &n.body)?;
        }
    }
    Ok(())
}

pub(super) fn execute_nodes(state: &mut ExecutionState<'_>, nodes: &[WorkflowNode]) -> Result<()> {
    for node in nodes {
        if !state.all_succeeded && state.exec_config.fail_fast {
            break;
        }
        execute_single_node(state, node, 0)?;
    }
    Ok(())
}

/// Record a failed step result and optionally return a fail-fast error.
pub(super) fn record_step_failure(
    state: &mut ExecutionState<'_>,
    step_key: String,
    step_label: &str,
    last_error: String,
    max_attempts: u32,
) -> Result<()> {
    state.all_succeeded = false;
    let step_result = StepResult {
        step_name: step_label.to_string(),
        status: WorkflowStepStatus::Failed,
        result_text: Some(last_error),
        cost_usd: None,
        num_turns: None,
        duration_ms: None,
        markers: Vec::new(),
        context: String::new(),
        child_run_id: None,
        structured_output: None,
        output_file: None,
    };
    state.step_results.insert(step_key, step_result);

    if state.exec_config.fail_fast {
        return Err(ConductorError::Workflow(format!(
            "Step '{}' failed after {} attempts",
            step_label, max_attempts
        )));
    }

    Ok(())
}

/// Record a successful step: accumulate stats, insert StepResult, push context.
#[allow(clippy::too_many_arguments)]
pub(super) fn record_step_success(
    state: &mut ExecutionState<'_>,
    step_key: String,
    step_name: &str,
    result_text: Option<String>,
    cost_usd: Option<f64>,
    num_turns: Option<i64>,
    duration_ms: Option<i64>,
    markers: Vec<String>,
    context: String,
    child_run_id: Option<String>,
    iteration: u32,
    structured_output: Option<String>,
    output_file: Option<String>,
) {
    if let Some(cost) = cost_usd {
        state.total_cost += cost;
    }
    if let Some(turns) = num_turns {
        state.total_turns += turns;
    }
    if let Some(dur) = duration_ms {
        state.total_duration_ms += dur;
    }

    let markers_for_ctx = markers.clone();
    let structured_output_for_ctx = structured_output.clone();
    let output_file_for_ctx = output_file.clone();
    let step_result = StepResult {
        step_name: step_name.to_string(),
        status: WorkflowStepStatus::Completed,
        result_text,
        cost_usd,
        num_turns,
        duration_ms,
        markers,
        context: context.clone(),
        child_run_id,
        structured_output,
        output_file,
    };
    state.step_results.insert(step_key, step_result);

    state.contexts.push(ContextEntry {
        step: step_name.to_string(),
        iteration,
        context,
        markers: markers_for_ctx,
        structured_output: structured_output_for_ctx,
        output_file: output_file_for_ctx,
    });
}

/// Resolve child workflow inputs: substitute variables, apply defaults, and
/// check for missing required inputs.
///
/// Returns `Ok(resolved_inputs)` or `Err(missing_input_name)`.
pub(super) fn resolve_child_inputs(
    raw_inputs: &HashMap<String, String>,
    vars: &HashMap<&str, String>,
    input_decls: &[workflow_dsl::InputDecl],
) -> std::result::Result<HashMap<String, String>, String> {
    let mut child_inputs = HashMap::new();
    for (k, v) in raw_inputs {
        child_inputs.insert(
            k.clone(),
            super::prompt_builder::substitute_variables(v, vars),
        );
    }
    for decl in input_decls {
        if !child_inputs.contains_key(&decl.name) {
            if decl.required {
                return Err(decl.name.clone());
            }
            if let Some(ref default) = decl.default {
                child_inputs.insert(decl.name.clone(), default.clone());
            }
            // Boolean inputs default to "false" when absent, matching
            // the behaviour of apply_workflow_input_defaults.
            if decl.input_type == workflow_dsl::InputType::Boolean {
                child_inputs
                    .entry(decl.name.clone())
                    .or_insert_with(|| "false".to_string());
            }
        }
    }
    Ok(child_inputs)
}

/// Run the on_fail agent after all retries for a step are exhausted.
///
/// Injects `failed_step`, `failure_reason`, and `retry_count` into the
/// workflow inputs for the duration of the on_fail call, then cleans up.
pub(super) fn run_on_fail_agent(
    state: &mut ExecutionState<'_>,
    step_label: &str,
    on_fail_agent: &crate::workflow_dsl::AgentRef,
    last_error: &str,
    retries: u32,
    iteration: u32,
) {
    tracing::warn!(
        "All retries exhausted for '{}', running on_fail agent '{}'",
        step_label,
        on_fail_agent.label(),
    );
    state
        .inputs
        .insert("failed_step".to_string(), step_label.to_string());
    state
        .inputs
        .insert("failure_reason".to_string(), last_error.to_string());
    state
        .inputs
        .insert("retry_count".to_string(), retries.to_string());

    let on_fail_node = crate::workflow_dsl::CallNode {
        agent: on_fail_agent.clone(),
        retries: 0,
        on_fail: None,
        output: None,
        with: Vec::new(),
        bot_name: None,
    };
    if let Err(e) = super::executors::execute_call(state, &on_fail_node, iteration) {
        tracing::warn!("on_fail agent '{}' also failed: {e}", on_fail_agent.label(),);
    }

    state.inputs.remove("failed_step");
    state.inputs.remove("failure_reason");
    state.inputs.remove("retry_count");
}

/// Check whether a step should be skipped on resume.
pub(super) fn should_skip(state: &ExecutionState<'_>, step_name: &str, iteration: u32) -> bool {
    state.resume_ctx.as_ref().is_some_and(|ctx| {
        ctx.skip_completed
            .contains(&(step_name.to_owned(), iteration))
    })
}

/// Temporarily take the `ResumeContext` out of `state` so we can borrow `state`
/// mutably while reading from the context's maps.
pub(super) fn restore_step(state: &mut ExecutionState<'_>, key: &str, iteration: u32) {
    let ctx = state.resume_ctx.take();
    if let Some(ref ctx) = ctx {
        restore_completed_step(state, ctx, key, iteration);
    }
    state.resume_ctx = ctx;
}

/// Restore a completed step's results from the resume context into the
/// execution state.
///
/// Rebuilds `step_results` and `contexts` for completed steps so that
/// downstream variable substitution (e.g. `{{prior_context}}`) works correctly.
pub(super) fn restore_completed_step(
    state: &mut ExecutionState<'_>,
    ctx: &ResumeContext,
    step_key: &str,
    iteration: u32,
) {
    let completed_step = ctx.step_map.get(&(step_key.to_owned(), iteration));

    let Some(step) = completed_step else {
        tracing::warn!(
            "resume: step '{step_key}:{iteration}' in skip set but not found in resume context \
             — downstream variable substitution may be incorrect"
        );
        return;
    };

    let markers: Vec<String> = step
        .markers_out
        .as_deref()
        .and_then(|m| {
            serde_json::from_str(m)
                .map_err(|e| {
                    tracing::warn!(
                        "resume: failed to deserialize markers for step '{}': {e}",
                        step_key
                    );
                    e
                })
                .ok()
        })
        .unwrap_or_default();
    let context = step.context_out.clone().unwrap_or_default();

    // Accumulate costs from the pre-loaded child agent run
    if let Some(ref child_run_id) = step.child_run_id {
        if let Some(run) = ctx.child_runs.get(child_run_id) {
            if let Some(cost) = run.cost_usd {
                state.total_cost += cost;
            }
            if let Some(turns) = run.num_turns {
                state.total_turns += turns;
            }
            if let Some(dur) = run.duration_ms {
                state.total_duration_ms += dur;
            }
        } else {
            tracing::warn!(
                "resume: child agent run '{child_run_id}' for step '{step_key}' not found \
                 — cost/turns/duration will be excluded from resumed run totals"
            );
        }
    }

    // Restore gate feedback if this was a gate step
    if let Some(ref feedback) = step.gate_feedback {
        state.last_gate_feedback = Some(feedback.clone());
    }

    let markers_for_ctx = markers.clone();
    let step_result = StepResult {
        step_name: step_key.to_string(),
        status: WorkflowStepStatus::Completed,
        result_text: step.result_text.clone(),
        cost_usd: None,
        num_turns: None,
        duration_ms: None,
        markers,
        context: context.clone(),
        child_run_id: step.child_run_id.clone(),
        structured_output: step.structured_output.clone(),
        output_file: step.output_file.clone(),
    };
    state.step_results.insert(step_key.to_string(), step_result);

    state.contexts.push(ContextEntry {
        step: step_key.to_string(),
        iteration,
        context,
        markers: markers_for_ctx,
        structured_output: step.structured_output.clone(),
        output_file: step.output_file.clone(),
    });
}

/// Fetch the final step's markers and context from a completed child workflow run.
#[cfg(test)]
pub(super) fn fetch_child_final_output(
    wf_mgr: &WorkflowManager<'_>,
    workflow_run_id: &str,
) -> (Vec<String>, String) {
    let steps = match wf_mgr.get_workflow_steps(workflow_run_id) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch steps for child workflow run '{}': {e}",
                workflow_run_id,
            );
            return (Vec::new(), String::new());
        }
    };

    // Find the last completed step (by position descending)
    let last_completed = steps
        .iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .max_by_key(|s| s.position);

    match last_completed {
        Some(step) => {
            let markers: Vec<String> = step
                .markers_out
                .as_deref()
                .map(|m| {
                    serde_json::from_str(m).unwrap_or_else(|e| {
                        tracing::warn!(
                            "Malformed markers_out JSON in step '{}': {e}",
                            step.step_name,
                        );
                        Vec::new()
                    })
                })
                .unwrap_or_default();
            let context = step.context_out.clone().unwrap_or_default();
            (markers, context)
        }
        None => (Vec::new(), String::new()),
    }
}

/// Fetch both the final step output (markers + context) and all completed step
/// results for a child workflow run in a single DB query.
///
/// This is a combined form of `fetch_child_final_output` +
/// `bubble_up_child_step_results` that avoids issuing two identical
/// `get_workflow_steps()` queries back-to-back.
pub(super) fn fetch_child_completion_data(
    wf_mgr: &WorkflowManager<'_>,
    workflow_run_id: &str,
) -> ((Vec<String>, String), HashMap<String, StepResult>) {
    let steps = match wf_mgr.get_workflow_steps(workflow_run_id) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch steps for child workflow run '{}': {e}",
                workflow_run_id,
            );
            return ((Vec::new(), String::new()), HashMap::new());
        }
    };

    // Derive final output from the last completed step.
    let last_completed = steps
        .iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .max_by_key(|s| s.position);

    let final_output = match last_completed {
        Some(step) => {
            let markers: Vec<String> = step
                .markers_out
                .as_deref()
                .map(|m| {
                    serde_json::from_str(m).unwrap_or_else(|e| {
                        tracing::warn!(
                            "Malformed markers_out JSON in step '{}': {e}",
                            step.step_name,
                        );
                        Vec::new()
                    })
                })
                .unwrap_or_default();
            let context = step.context_out.clone().unwrap_or_default();
            (markers, context)
        }
        None => (Vec::new(), String::new()),
    };

    // Build bubble-up map from all completed steps.
    let child_steps = steps
        .into_iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .map(|s| {
            let markers: Vec<String> = s
                .markers_out
                .as_deref()
                .map(|m| {
                    serde_json::from_str(m).unwrap_or_else(|e| {
                        tracing::warn!(
                            "Malformed markers_out JSON in child step '{}': {e}",
                            s.step_name,
                        );
                        Vec::new()
                    })
                })
                .unwrap_or_default();
            let context = s.context_out.clone().unwrap_or_default();
            let result = StepResult {
                step_name: s.step_name.clone(),
                status: WorkflowStepStatus::Completed,
                result_text: s.result_text.clone(),
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                markers,
                context,
                child_run_id: s.child_run_id.clone(),
                structured_output: s.structured_output.clone(),
                output_file: s.output_file.clone(),
            };
            (s.step_name, result)
        })
        .collect();

    (final_output, child_steps)
}

/// Fetch all completed child steps and build minimal `StepResult` objects for
/// merging into the parent's `step_results` map.
#[cfg(test)]
pub(super) fn bubble_up_child_step_results(
    wf_mgr: &WorkflowManager<'_>,
    workflow_run_id: &str,
) -> HashMap<String, StepResult> {
    let steps = match wf_mgr.get_workflow_steps(workflow_run_id) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch steps for child workflow run '{}' during bubble-up: {e}",
                workflow_run_id,
            );
            return HashMap::new();
        }
    };

    steps
        .into_iter()
        .filter(|s| s.status == WorkflowStepStatus::Completed)
        .map(|s| {
            let markers: Vec<String> = s
                .markers_out
                .as_deref()
                .map(|m| {
                    serde_json::from_str(m).unwrap_or_else(|e| {
                        tracing::warn!(
                            "Malformed markers_out JSON in child step '{}': {e}",
                            s.step_name,
                        );
                        Vec::new()
                    })
                })
                .unwrap_or_default();
            let context = s.context_out.clone().unwrap_or_default();
            let result = StepResult {
                step_name: s.step_name.clone(),
                status: WorkflowStepStatus::Completed,
                result_text: s.result_text.clone(),
                cost_usd: None,
                num_turns: None,
                duration_ms: None,
                markers,
                context,
                child_run_id: s.child_run_id.clone(),
                structured_output: s.structured_output.clone(),
                output_file: s.output_file.clone(),
            };
            (s.step_name, result)
        })
        .collect()
}

/// Check whether the loop is stuck (identical marker sets for `stuck_after` consecutive
/// iterations). Returns `Err` if stuck, `Ok(())` otherwise.
pub(super) fn check_stuck(
    state: &mut ExecutionState<'_>,
    prev_marker_sets: &mut Vec<HashSet<String>>,
    step: &str,
    marker: &str,
    stuck_after: u32,
    loop_kind: &str,
) -> Result<()> {
    let current_markers: HashSet<String> = state
        .step_results
        .get(step)
        .map(|r| r.markers.iter().cloned().collect())
        .unwrap_or_default();

    prev_marker_sets.push(current_markers.clone());

    if prev_marker_sets.len() >= stuck_after as usize {
        let window = &prev_marker_sets[prev_marker_sets.len() - stuck_after as usize..];
        if window.iter().all(|s| s == &current_markers) {
            tracing::warn!(
                "{loop_kind} {step}.{marker} — stuck: identical markers for {stuck_after} consecutive iterations",
            );
            state.all_succeeded = false;
            return Err(ConductorError::Workflow(format!(
                "{loop_kind} {step}.{marker} stuck after {stuck_after} iterations with identical markers",
            )));
        }
    }

    Ok(())
}

/// Check whether the loop has exceeded `max_iterations`. Returns `Ok(true)` if the caller
/// should break out of the loop (`on_max_iter = continue`), `Ok(false)` to keep going,
/// or `Err` if `on_max_iter = fail`.
pub(super) fn check_max_iterations(
    state: &mut ExecutionState<'_>,
    iteration: u32,
    max_iterations: u32,
    on_max_iter: &crate::workflow_dsl::OnMaxIter,
    step: &str,
    marker: &str,
    loop_kind: &str,
) -> Result<bool> {
    if iteration >= max_iterations {
        tracing::warn!("{loop_kind} {step}.{marker} — reached max_iterations ({max_iterations})",);
        match on_max_iter {
            crate::workflow_dsl::OnMaxIter::Fail => {
                state.all_succeeded = false;
                return Err(ConductorError::Workflow(format!(
                    "{loop_kind} {step}.{marker} reached max_iterations ({max_iterations})",
                )));
            }
            crate::workflow_dsl::OnMaxIter::Continue => return Ok(true),
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_dsl::{InputDecl, InputType, WorkflowDef, WorkflowTrigger};

    fn make_bool_workflow(
        name: &str,
        input_name: &str,
        required: bool,
        default: Option<&str>,
    ) -> WorkflowDef {
        WorkflowDef {
            name: name.to_string(),
            description: String::new(),
            trigger: WorkflowTrigger::Manual,
            targets: vec![],
            inputs: vec![InputDecl {
                name: input_name.to_string(),
                input_type: InputType::Boolean,
                required,
                default: default.map(|s| s.to_string()),
                description: None,
            }],
            body: vec![],
            always: vec![],
            source_path: String::new(),
        }
    }

    #[test]
    fn test_boolean_input_defaults_to_false_when_absent() {
        let workflow = make_bool_workflow("wf", "flag", false, None);
        let mut inputs = HashMap::new();
        apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
        assert_eq!(inputs.get("flag").map(|s| s.as_str()), Some("false"));
    }

    #[test]
    fn test_boolean_input_uses_explicit_default_over_false() {
        let workflow = make_bool_workflow("wf", "flag", false, Some("true"));
        let mut inputs = HashMap::new();
        apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
        assert_eq!(inputs.get("flag").map(|s| s.as_str()), Some("true"));
    }

    #[test]
    fn test_boolean_input_caller_value_not_overwritten() {
        let workflow = make_bool_workflow("wf", "flag", false, None);
        let mut inputs = HashMap::new();
        inputs.insert("flag".to_string(), "true".to_string());
        apply_workflow_input_defaults(&workflow, &mut inputs).unwrap();
        // Caller's value wins — default ("false") must not overwrite it.
        assert_eq!(inputs.get("flag").map(|s| s.as_str()), Some("true"));
    }

    #[test]
    fn test_boolean_input_required_and_missing_is_error() {
        let workflow = make_bool_workflow("wf", "flag", true, None);
        let mut inputs = HashMap::new();
        // Required but not provided — should return an error before defaulting.
        let result = apply_workflow_input_defaults(&workflow, &mut inputs);
        assert!(result.is_err(), "expected error for missing required input");
    }
}
