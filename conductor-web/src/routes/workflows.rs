use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::error::ConductorError;
use conductor_core::repo::RepoManager;
use conductor_core::workflow::{
    apply_workflow_input_defaults, estimation, execute_workflow, validate_resume_preconditions,
    FanOutItemRow, GateAnalyticsRow, InputDecl, PendingGateAnalyticsRow, RunIdSlot,
    StepFailureHeatmapRow, StepRetryAnalyticsRow, StepTokenHeatmapRow, TimeGranularity,
    WorkflowDef, WorkflowExecConfig, WorkflowExecInput, WorkflowFailureRateTrendRow,
    WorkflowManager, WorkflowPercentiles, WorkflowRegressionSignal, WorkflowResumeStandalone,
    WorkflowRun, WorkflowRunMetricsRow, WorkflowRunStatus, WorkflowRunStep, WorkflowStepStatus,
    WorkflowTokenAggregate, WorkflowTokenTrendRow, REGRESSION_MIN_RECENT_RUNS,
};
use conductor_core::worktree::WorktreeManager;

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::notify::{fire_workflow_notification, WorkflowNotificationArgs};
use crate::state::AppState;

/// Parse granularity from query parameter with default fallback.
/// Returns a parsed TimeGranularity or an error for invalid values.
fn parse_granularity(granularity: Option<String>) -> Result<TimeGranularity, ApiError> {
    granularity
        .as_deref()
        .unwrap_or("daily")
        .parse()
        .map_err(|e: String| ApiError::Core(ConductorError::InvalidInput(e)))
}

/// Resolve the run ID to use for error-path notifications.
///
/// When `execute_workflow` created a run record before failing, the slot holds
/// the real ULID. Use it so dedup aligns with any concurrent TUI notification
/// for the same run. Fall back to the deterministic per-minute bucket key only
/// when no run record was created at all (pre-creation failure).
fn resolve_error_run_id(slot: &RunIdSlot, wf_name: &str, label: &str) -> String {
    let captured = slot.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match captured {
        Some(id) => id,
        None => {
            let bucket = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                / 60;
            format!("wf-err:{wf_name}:{label}:{bucket}")
        }
    }
}

/// Wait for `run_id_slot` to be populated by the spawned workflow task, or
/// time out after 5 seconds.
///
/// Uses `spawn_blocking` because `RunIdSlot` is built on `std::sync::Condvar`,
/// which requires a blocking thread (it cannot be awaited directly in async
/// code without parking the async runtime).
///
/// Returns `None` if the workflow task didn't write a run ID within 5 seconds
/// or if the mutex was poisoned.
async fn wait_for_run_id(slot: RunIdSlot) -> Option<String> {
    tokio::task::spawn_blocking(move || {
        let (lock, cvar) = &*slot;
        let guard = match lock.lock() {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!("run_id_slot mutex poisoned; run_id will be null");
                e.into_inner()
            }
        };
        let (guard, timed_out) = cvar
            .wait_timeout_while(guard, std::time::Duration::from_secs(5), |id| id.is_none())
            .unwrap_or_else(|e| e.into_inner());
        if timed_out.timed_out() {
            tracing::warn!("timed out waiting for run_id; run_id will be null in response");
        }
        guard.clone()
    })
    .await
    .unwrap_or(None)
}

/// Fire a workflow completion notification.
///
/// # Calling context
///
/// **Must only be called from a synchronous/blocking context** — i.e. inside
/// `tokio::task::spawn_blocking` or a plain OS thread — because
/// `open_database` is a synchronous call.
fn notify_workflow(
    conn: &rusqlite::Connection,
    notifications: &conductor_core::config::NotificationConfig,
    notify_hooks: &[conductor_core::config::HookConfig],
    params: &WorkflowNotificationArgs<'_>,
) {
    fire_workflow_notification(conn, notifications, notify_hooks, params);
}

// ── Response types ────────────────────────────────────────────────────

/// Web-layer wrapper that attaches active steps to a `WorkflowRun` for the list endpoint.
/// Preserves the exact JSON shape the frontend expects (active_steps is omitted when empty).
#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkflowRunResponse {
    #[serde(flatten)]
    run: WorkflowRun,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    active_steps: Vec<WorkflowRunStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree_slug: Option<String>,
    /// Total number of steps in the workflow definition (from definition_snapshot).
    #[serde(skip_serializing_if = "Option::is_none")]
    total_steps: Option<usize>,
    /// Position of the current (or last completed/failed) step (1-indexed).
    #[serde(skip_serializing_if = "Option::is_none")]
    current_step: Option<i64>,
    /// Name of the current active step (for display).
    #[serde(skip_serializing_if = "Option::is_none")]
    current_step_name: Option<String>,
    /// Current iteration for do-while loops (0 = first pass).
    #[serde(skip_serializing_if = "Option::is_none")]
    current_iteration: Option<i64>,
    /// Max iterations configured for the active do-while loop, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_iterations: Option<i64>,
    /// Estimated total workflow duration in milliseconds (hybrid: LLM + historical).
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_duration_ms: Option<i64>,
    /// Estimated remaining milliseconds for in-progress runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_remaining_ms: Option<i64>,
    /// Confidence level for the time estimate.
    #[serde(skip_serializing_if = "Option::is_none")]
    estimate_confidence: Option<conductor_core::workflow::Confidence>,
    /// Lower bound (p25) of estimated remaining ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_remaining_low_ms: Option<i64>,
    /// Upper bound (p75) of estimated remaining ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_remaining_high_ms: Option<i64>,
    /// Per-step time estimates for active runs (step_name → estimate).
    #[serde(skip_serializing_if = "Option::is_none")]
    step_estimates: Option<HashMap<String, conductor_core::workflow::Estimate>>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct InputDeclSummary {
    pub name: String,
    pub required: bool,
    #[serde(rename = "type")]
    pub input_type: String,
    #[serde(rename = "defaultValue")]
    pub default: Option<String>,
    pub description: Option<String>,
}

impl From<&InputDecl> for InputDeclSummary {
    fn from(d: &InputDecl) -> Self {
        use conductor_core::workflow::InputType;
        Self {
            name: d.name.clone(),
            required: d.required,
            input_type: match d.input_type {
                InputType::Boolean => "boolean".to_string(),
                InputType::String => "string".to_string(),
            },
            default: d.default.clone(),
            description: d.description.clone(),
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkflowDefSummary {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub trigger: String,
    pub inputs: Vec<InputDeclSummary>,
    pub node_count: usize,
    pub group: Option<String>,
    pub targets: Vec<String>,
    /// Whether the workflow definition is valid (parsed and passed validation).
    pub valid: bool,
    /// Human-readable error message if `valid` is false.
    pub error: Option<String>,
}

impl From<&WorkflowDef> for WorkflowDefSummary {
    fn from(def: &WorkflowDef) -> Self {
        Self {
            name: def.name.clone(),
            title: def.title.clone(),
            description: def.description.clone(),
            trigger: def.trigger.to_string(),
            inputs: def.inputs.iter().map(InputDeclSummary::from).collect(),
            node_count: def.top_level_steps(),
            group: def.group.clone(),
            targets: def.targets.clone(),
            valid: true,
            error: None,
        }
    }
}

// ── Request types ─────────────────────────────────────────────────────

/// Filter for workflow listing endpoints.
#[derive(Deserialize, utoipa::ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum StatusFilter {
    /// Return all workflows (valid and invalid). This is the default.
    #[default]
    All,
    /// Return only successfully-parsed and validated workflows.
    Valid,
    /// Return only workflows that failed to parse or failed validation.
    Invalid,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct WorkflowListParams {
    /// Filter by validity status. Defaults to `all`.
    pub status: Option<StatusFilter>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RunWorkflowRequest {
    pub name: String,
    pub model: Option<String>,
    pub dry_run: Option<bool>,
    pub inputs: Option<HashMap<String, String>>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct PostWorkflowRunRequest {
    pub repo: String,
    pub workflow: String,
    pub worktree: Option<String>,
    pub ticket_id: Option<String>,
    pub inputs: Option<HashMap<String, String>>,
    pub dry_run: Option<bool>,
    pub model: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ResumeWorkflowRequest {
    pub from_step: Option<String>,
    pub model: Option<String>,
    pub restart: Option<bool>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct GateActionRequest {
    pub feedback: Option<String>,
    pub selections: Option<Vec<String>>,
}

// ── Endpoints ─────────────────────────────────────────────────────────

/// Build workflow definition summaries from a worktree+repo path pair.
///
/// Combines parse-failure warnings and post-parse batch validation into a
/// single sorted `Vec<WorkflowDefSummary>`. Uses `known_bots` derived from
/// the caller's config so bot-name validation matches the CLI.
fn build_workflow_summaries(
    wt_path: &str,
    repo_path: &str,
    known_bots: &std::collections::HashSet<String>,
) -> Result<Vec<WorkflowDefSummary>, ApiError> {
    let (defs, invalid_entries) =
        WorkflowManager::list_defs_with_validation(wt_path, repo_path, known_bots)
            .map_err(ApiError::Core)?;

    let mut summaries: Vec<WorkflowDefSummary> = Vec::new();

    // Convert valid defs to summaries.
    for def in &defs {
        summaries.push(WorkflowDefSummary::from(def));
    }

    // Convert invalid entries to summaries.
    for entry in &invalid_entries {
        summaries.push(WorkflowDefSummary {
            name: entry.name.clone(),
            title: None,
            description: String::new(),
            trigger: String::new(),
            inputs: Vec::new(),
            node_count: 0,
            group: None,
            targets: Vec::new(),
            valid: false,
            error: Some(entry.error.clone()),
        });
    }

    // Sort alphabetically by name for a consistent ordering.
    summaries.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(summaries)
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/workflows",
    params(
        ("id" = String, Path, description = "Repo ID"),
        ("status" = Option<StatusFilter>, Query, description = "Filter by validity: valid, invalid, or all (default)"),
    ),
    responses(
        (status = 200, description = "List of workflow definitions for repo (includes invalid entries)", body = Vec<WorkflowDefSummary>),
        (status = 404, description = "Repo not found"),
    ),
    tag = "workflows",
)]
/// GET /api/repos/{id}/workflows
pub async fn list_repo_workflow_defs(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    Query(params): Query<WorkflowListParams>,
) -> Result<Json<Vec<WorkflowDefSummary>>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let repo = RepoManager::new(&db, &config).get_by_id(&repo_id)?;

    let known_bots: std::collections::HashSet<String> =
        config.github.apps.keys().cloned().collect();
    let summaries = build_workflow_summaries("", &repo.local_path, &known_bots)?;

    let result = match params.status.unwrap_or_default() {
        StatusFilter::All => summaries,
        StatusFilter::Valid => summaries.into_iter().filter(|s| s.valid).collect(),
        StatusFilter::Invalid => summaries.into_iter().filter(|s| !s.valid).collect(),
    };

    Ok(Json(result))
}

#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/workflows/defs",
    params(
        ("id" = String, Path, description = "Worktree ID"),
        ("status" = Option<StatusFilter>, Query, description = "Filter by validity: valid, invalid, or all (default)"),
    ),
    responses(
        (status = 200, description = "List of workflow definitions for worktree (includes invalid entries)", body = Vec<WorkflowDefSummary>),
        (status = 404, description = "Worktree not found"),
    ),
    tag = "workflows",
)]
/// GET /api/worktrees/{id}/workflows/defs
pub async fn list_workflow_defs(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
    Query(params): Query<WorkflowListParams>,
) -> Result<Json<Vec<WorkflowDefSummary>>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let wt_mgr = WorktreeManager::new(&db, &config);
    let wt = wt_mgr.get_by_id(&worktree_id)?;
    let repo = RepoManager::new(&db, &config).get_by_id(&wt.repo_id)?;

    let known_bots: std::collections::HashSet<String> =
        config.github.apps.keys().cloned().collect();
    let summaries = build_workflow_summaries(&wt.path, &repo.local_path, &known_bots)?;

    let result = match params.status.unwrap_or_default() {
        StatusFilter::All => summaries,
        StatusFilter::Valid => summaries.into_iter().filter(|s| s.valid).collect(),
        StatusFilter::Invalid => summaries.into_iter().filter(|s| !s.valid).collect(),
    };

    Ok(Json(result))
}

#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/workflows/defs/{name}",
    params(
        ("id" = String, Path, description = "Worktree ID"),
        ("name" = String, Path, description = "Workflow definition name"),
    ),
    responses(
        (status = 200, description = "Workflow definition"),
        (status = 404, description = "Worktree or workflow not found"),
    ),
    tag = "workflows",
)]
/// GET /api/worktrees/{id}/workflows/defs/{name}
pub async fn get_workflow_def(
    State(state): State<AppState>,
    Path((worktree_id, def_name)): Path<(String, String)>,
) -> Result<Json<WorkflowDef>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let wt_mgr = WorktreeManager::new(&db, &config);
    let wt = wt_mgr.get_by_id(&worktree_id)?;
    let repo = RepoManager::new(&db, &config).get_by_id(&wt.repo_id)?;

    let (defs, _warnings) =
        WorkflowManager::list_defs(&wt.path, &repo.local_path).map_err(ApiError::Core)?;

    let def = defs
        .into_iter()
        .find(|d| d.name == def_name)
        .ok_or_else(|| {
            ApiError::from(ConductorError::Workflow(format!(
                "Workflow definition '{}' not found",
                def_name
            )))
        })?;

    Ok(Json(def))
}

#[utoipa::path(
    post,
    path = "/api/worktrees/{id}/workflows/run",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    request_body(content = RunWorkflowRequest, description = "Workflow run parameters"),
    responses(
        (status = 202, description = "Workflow started"),
        (status = 404, description = "Worktree or workflow not found"),
    ),
    tag = "workflows",
)]
/// POST /api/worktrees/{id}/workflows/run
pub async fn run_workflow(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
    Json(req): Json<RunWorkflowRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // Validate inputs while holding the lock
    let (wt_path, wt_slug, wt_ticket_id, repo_path, repo_slug, repo_id, model) = {
        let db = state.db.lock().await;
        let config = state.config.read().await;
        let wt_mgr = WorktreeManager::new(&db, &config);
        let repo_mgr = RepoManager::new(&db, &config);

        let wt = wt_mgr.get_by_id(&worktree_id)?;
        let repo = repo_mgr.get_by_id(&wt.repo_id)?;

        // Validate workflow exists
        let _def = WorkflowManager::load_def_by_name(&wt.path, &repo.local_path, &req.name)?;

        // Reject if a top-level workflow run is already active on this worktree
        let wf_mgr = WorkflowManager::new(&db);
        if let Some(active) = wf_mgr.get_active_run_for_worktree(&worktree_id)? {
            return Err(ApiError::Core(ConductorError::WorkflowRunAlreadyActive {
                name: active.workflow_name,
            }));
        }

        // Resolve model: request → per-worktree → per-repo → global config
        let model = req
            .model
            .clone()
            .or_else(|| wt.model.clone())
            .or_else(|| repo.model.clone())
            .or_else(|| config.general.model.clone());

        (
            wt.path.clone(),
            wt.slug.clone(),
            wt.ticket_id.clone(),
            repo.local_path.clone(),
            repo.slug.clone(),
            repo.id.clone(),
            model,
        )
    };

    let workflow_name = req.name.clone();
    let dry_run = req.dry_run.unwrap_or(false);
    let mut inputs = req.inputs.unwrap_or_default();
    let wt_id = worktree_id.clone();

    // Spawn blocking task with its own DB connection so the shared AppState
    // mutex is not held for the entire workflow execution (which would starve
    // all other API requests).
    let wt_target_label = format!("{repo_slug}/{wt_slug}");
    let config = state.config.read().await.clone();
    let notifications = config.notifications.clone();
    let notify_hooks = config.notify.hooks.clone();
    // Slot receives the real workflow run ULID once execute_workflow creates the
    // DB record. On the error path we prefer the real ULID (so dedup aligns with
    // any concurrent TUI notification keyed on the same ID); we fall back to the
    // deterministic bucket key only when no run record was created at all.
    let run_id_slot: RunIdSlot =
        std::sync::Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));
    let response_slot = std::sync::Arc::clone(&run_id_slot);
    let state_clone = state.clone();
    let db_path = state.db_path.clone();
    tokio::task::spawn_blocking(move || {
        let def = match WorkflowManager::load_def_by_name(&wt_path, &repo_path, &workflow_name) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("Failed to load workflow def: {e}");
                return;
            }
        };

        // Validate required inputs and apply defaults (matches CLI and ephemeral paths)
        if let Err(e) = apply_workflow_input_defaults(&def, &mut inputs) {
            tracing::error!("Workflow input validation failed: {e}");
            return;
        }

        let params = conductor_core::workflow::WorkflowExecStandalone {
            config,
            workflow: def,
            worktree_id: Some(wt_id),
            working_dir: wt_path,
            repo_path,
            ticket_id: wt_ticket_id,
            repo_id: None,
            model,
            exec_config: WorkflowExecConfig {
                dry_run,
                ..Default::default()
            },
            inputs,
            target_label: Some(wt_target_label.clone()),
            run_id_notify: Some(std::sync::Arc::clone(&run_id_slot)),
            triggered_by_hook: false,
            conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
            force: false,
            extra_plugin_dirs: vec![],
            db_path: Some(db_path.clone()),
            parent_workflow_run_id: None,
        };

        let result = conductor_core::workflow::execute_workflow_standalone(&params);

        // Use the same db_path as the workflow execution for consistency
        let notification_conn = conductor_core::db::open_database(&db_path);

        // Always emit events and notify, even if DB operations fail
        match result {
            Ok(res) => {
                let succeeded = res.all_succeeded;
                let status = if succeeded { "completed" } else { "failed" };

                // Send notification if DB connection is available
                if let Ok(conn) = &notification_conn {
                    notify_workflow(
                        conn,
                        &notifications,
                        &notify_hooks,
                        &WorkflowNotificationArgs {
                            run_id: &res.workflow_run_id,
                            workflow_name: &workflow_name,
                            target_label: Some(&wt_target_label),
                            succeeded,
                            parent_workflow_run_id: None, // workflows launched from web are always root runs
                            repo_slug: &repo_slug,
                            branch: &wt_slug,
                            duration_ms: None,
                            ticket_url: None,
                            error: None,
                            repo_id: Some(&repo_id),
                            worktree_id: params.worktree_id.as_deref(),
                        },
                    );
                } else if let Err(e) = &notification_conn {
                    tracing::error!("notify: DB open failed, skipping notification: {e}");
                }

                state_clone
                    .events
                    .emit(ConductorEvent::WorkflowRunStatusChanged {
                        run_id: res.workflow_run_id,
                        worktree_id: res.worktree_id,
                        status: status.to_string(),
                    });
            }
            Err(e) => {
                tracing::error!("Workflow execution failed: {e}");
                let error_run_id =
                    resolve_error_run_id(&run_id_slot, &workflow_name, &wt_target_label);

                // Send notification if DB connection is available
                if let Ok(conn) = &notification_conn {
                    notify_workflow(
                        conn,
                        &notifications,
                        &notify_hooks,
                        &WorkflowNotificationArgs {
                            run_id: &error_run_id,
                            workflow_name: &workflow_name,
                            target_label: Some(&wt_target_label),
                            succeeded: false,
                            parent_workflow_run_id: None, // workflows launched from web are always root runs
                            repo_slug: &repo_slug,
                            branch: &wt_slug,
                            duration_ms: None,
                            ticket_url: None,
                            error: None,
                            repo_id: Some(&repo_id),
                            worktree_id: params.worktree_id.as_deref(),
                        },
                    );
                } else if let Err(e) = &notification_conn {
                    tracing::error!("notify: DB open failed, skipping notification: {e}");
                }
            }
        }

        if let Some(notify) = &state_clone.workflow_done_notify {
            notify.notify_one();
        }
    });

    let run_id = wait_for_run_id(response_slot).await;

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "started",
            "worktree_id": worktree_id,
            "run_id": run_id,
        })),
    ))
}

fn check_no_active_run(wf_mgr: &WorkflowManager<'_>, wt_id: &str) -> Result<(), ApiError> {
    if let Some(active) = wf_mgr.get_active_run_for_worktree(wt_id)? {
        return Err(ApiError::Core(ConductorError::WorkflowRunAlreadyActive {
            name: active.workflow_name,
        }));
    }
    Ok(())
}

#[utoipa::path(
    post,
    path = "/api/workflows/runs",
    request_body(content = PostWorkflowRunRequest, description = "Workflow run parameters"),
    responses(
        (status = 202, description = "Workflow started"),
        (status = 404, description = "Repo or workflow not found"),
    ),
    tag = "workflows",
)]
/// POST /api/workflows/runs
pub async fn post_workflow_run(
    State(state): State<AppState>,
    Json(req): Json<PostWorkflowRunRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // Validate inputs while holding the lock
    let (wt_path, wt_slug, wt_ticket_id, repo_path, repo_slug, repo_id, resolved_wt_id, model, def) = {
        let db = state.db.lock().await;
        let config = state.config.read().await;
        let wt_mgr = WorktreeManager::new(&db, &config);
        let repo_mgr = RepoManager::new(&db, &config);

        // Resolve repo by ULID first, fall back to slug
        let repo = match repo_mgr.get_by_id(&req.repo) {
            Ok(r) => r,
            Err(conductor_core::error::ConductorError::RepoNotFound { .. }) => {
                repo_mgr.get_by_slug(&req.repo)?
            }
            Err(e) => return Err(ApiError::Core(e)),
        };

        // Route based on which target fields are present
        let wf_mgr = WorkflowManager::new(&db);
        let (wt_path, wt_slug, wt_ticket_id, resolved_wt_id, wt_model) =
            if let Some(ref wt_id) = req.worktree {
                // Worktree path: validate ownership
                let wt = wt_mgr.get_by_id_for_repo(wt_id, &repo.id)?;

                // Reject if a top-level workflow run is already active on this worktree
                check_no_active_run(&wf_mgr, wt_id)?;

                let path = wt.path.clone();
                let slug = wt.slug.clone();
                let ticket_id = wt.ticket_id.clone();
                let wt_model = wt.model.clone();
                let wt_id = wt.id.clone();
                (path, slug, ticket_id, Some(wt_id), wt_model)
            } else if let Some(ref ticket_id) = req.ticket_id {
                // Ticket path: find an active worktree for this ticket in this repo
                let worktrees = wt_mgr.list_by_ticket(ticket_id)?;
                let active_wt = worktrees
                    .into_iter()
                    .find(|wt| wt.repo_id == repo.id && wt.is_active())
                    .ok_or_else(|| {
                        ApiError::Core(ConductorError::InvalidInput(format!(
                            "no active worktree found for ticket {ticket_id} in repo {}",
                            repo.slug
                        )))
                    })?;

                // Reject if a top-level workflow run is already active on this worktree
                check_no_active_run(&wf_mgr, &active_wt.id)?;

                let path = active_wt.path.clone();
                let slug = active_wt.slug.clone();
                let t_id = active_wt.ticket_id.clone();
                let wt_model = active_wt.model.clone();
                let wt_id = active_wt.id.clone();
                (path, slug, t_id, Some(wt_id), wt_model)
            } else {
                // Repo-only path: no worktree context
                // TODO: add get_active_run_for_repo() guard when WorkflowManager supports it
                (repo.local_path.clone(), repo.slug.clone(), None, None, None)
            };

        // Validate workflow exists (def is reused by the spawn below — no double load)
        let def = WorkflowManager::load_def_by_name(&wt_path, &repo.local_path, &req.workflow)?;
        let model = req
            .model
            .clone()
            .or(wt_model)
            .or_else(|| repo.model.clone())
            .or_else(|| config.general.model.clone());

        (
            wt_path,
            wt_slug,
            wt_ticket_id,
            repo.local_path.clone(),
            repo.slug.clone(),
            repo.id.clone(),
            resolved_wt_id,
            model,
            def,
        )
    };

    let workflow_name = req.workflow.clone();
    let dry_run = req.dry_run.unwrap_or(false);
    let mut inputs = req.inputs.unwrap_or_default();
    let wt_id_clone = resolved_wt_id.clone();
    let repo_id_for_response = repo_id.clone();

    let target_label = match &resolved_wt_id {
        Some(_) => format!("{repo_slug}/{wt_slug}"),
        None => repo_slug.clone(),
    };

    let run_id_slot: RunIdSlot =
        std::sync::Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));
    let response_slot = std::sync::Arc::clone(&run_id_slot);
    let config = state.config.read().await.clone();
    let db_path = state.db_path.clone();
    let state_clone = state.clone();
    tokio::task::spawn_blocking(move || {
        // Helper: emit a failed WorkflowRunStatusChanged event and return the run_id used.
        let emit_failed = |run_id_slot: &RunIdSlot, wt_id: Option<String>| -> String {
            let error_run_id = resolve_error_run_id(run_id_slot, &workflow_name, &target_label);
            state_clone
                .events
                .emit(ConductorEvent::WorkflowRunStatusChanged {
                    run_id: error_run_id.clone(),
                    worktree_id: wt_id,
                    status: "failed".to_string(),
                });
            error_run_id
        };

        let conn = match conductor_core::db::open_database(&db_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("DB open failed workflow={workflow_name}: {e}");
                emit_failed(&run_id_slot, wt_id_clone.clone());
                if let Some(notify) = &state_clone.workflow_done_notify {
                    notify.notify_one();
                }
                return;
            }
        };

        if let Err(e) = apply_workflow_input_defaults(&def, &mut inputs) {
            tracing::error!("Workflow input validation failed workflow={workflow_name}: {e}");
            emit_failed(&run_id_slot, wt_id_clone.clone());
            if let Some(notify) = &state_clone.workflow_done_notify {
                notify.notify_one();
            }
            return;
        }

        let exec_config = WorkflowExecConfig {
            dry_run,
            ..Default::default()
        };

        let input = WorkflowExecInput {
            conn: &conn,
            config: &config,
            workflow: &def,
            worktree_id: wt_id_clone.as_deref(),
            working_dir: &wt_path,
            repo_path: &repo_path,
            ticket_id: wt_ticket_id.as_deref(),
            repo_id: if wt_id_clone.is_none() {
                Some(&repo_id)
            } else {
                None
            },
            model: model.as_deref(),
            exec_config: &exec_config,
            inputs: inputs.clone(),
            depth: 0,
            parent_workflow_run_id: None,
            target_label: Some(&target_label),
            default_bot_name: None,
            iteration: 0,
            run_id_notify: Some(std::sync::Arc::clone(&run_id_slot)),
            triggered_by_hook: false,
            conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
            extra_plugin_dirs: vec![],
            force: false,
            parent_step_id: None,
        };

        let result = execute_workflow(&input);
        let notifications = config.notifications.clone();
        let notify_hooks = config.notify.hooks.clone();

        match result {
            Ok(res) => {
                let succeeded = res.all_succeeded;
                let status = if succeeded { "completed" } else { "failed" };

                let (notify_repo_slug, notify_branch) =
                    conductor_core::notify::parse_target_label(Some(&target_label));
                notify_workflow(
                    &conn,
                    &notifications,
                    &notify_hooks,
                    &WorkflowNotificationArgs {
                        run_id: &res.workflow_run_id,
                        workflow_name: &workflow_name,
                        target_label: Some(&target_label),
                        succeeded,
                        parent_workflow_run_id: None, // workflows launched from web are always root runs
                        repo_slug: notify_repo_slug,
                        branch: notify_branch,
                        duration_ms: None,
                        ticket_url: None,
                        error: None,
                        repo_id: Some(&repo_id),
                        worktree_id: wt_id_clone.as_deref(),
                    },
                );

                state_clone
                    .events
                    .emit(ConductorEvent::WorkflowRunStatusChanged {
                        run_id: res.workflow_run_id,
                        worktree_id: res.worktree_id,
                        status: status.to_string(),
                    });
            }
            Err(e) => {
                tracing::error!(
                    "Workflow execution failed workflow={workflow_name} target={target_label}: {e}"
                );
                let error_run_id = emit_failed(&run_id_slot, wt_id_clone.clone());
                let (notify_repo_slug, notify_branch) =
                    conductor_core::notify::parse_target_label(Some(&target_label));
                notify_workflow(
                    &conn,
                    &notifications,
                    &notify_hooks,
                    &WorkflowNotificationArgs {
                        run_id: &error_run_id,
                        workflow_name: &workflow_name,
                        target_label: Some(&target_label),
                        succeeded: false,
                        parent_workflow_run_id: None, // workflows launched from web are always root runs
                        repo_slug: notify_repo_slug,
                        branch: notify_branch,
                        duration_ms: None,
                        ticket_url: None,
                        error: None,
                        repo_id: Some(&repo_id),
                        worktree_id: wt_id_clone.as_deref(),
                    },
                );
            }
        }

        if let Some(notify) = &state_clone.workflow_done_notify {
            notify.notify_one();
        }
    });

    let run_id = wait_for_run_id(response_slot).await;

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "started",
            "worktree_id": resolved_wt_id,
            "repo_id": repo_id_for_response,
            "run_id": run_id,
        })),
    ))
}

/// Query params for GET /api/workflows/runs
#[derive(Deserialize, utoipa::IntoParams)]
pub struct ListAllRunsQuery {
    /// Comma-separated list of statuses. Defaults to running, waiting, pending (owned by the manager layer).
    pub status: Option<String>,
    /// Filter by repo slug. When provided, only runs associated with this repo are returned.
    pub repo: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/workflows/runs",
    params(ListAllRunsQuery),
    responses(
        (status = 200, description = "List of workflow runs", body = Vec<WorkflowRunResponse>),
    ),
    tag = "workflows",
)]
/// GET /api/workflows/runs?status=<csv>
pub async fn list_all_workflow_runs_handler(
    State(state): State<AppState>,
    Query(params): Query<ListAllRunsQuery>,
) -> Result<Json<Vec<WorkflowRunResponse>>, ApiError> {
    use std::str::FromStr;

    let raw = params.status.as_deref().unwrap_or("");
    let statuses: Vec<WorkflowRunStatus> = if raw.is_empty() {
        vec![]
    } else {
        raw.split(',')
            .map(|token| {
                let trimmed = token.trim();
                if trimmed.is_empty() {
                    return Err(ApiError::Core(ConductorError::InvalidInput(
                        "empty status token in list".to_string(),
                    )));
                }
                WorkflowRunStatus::from_str(trimmed)
                    .map_err(|e| ApiError::Core(ConductorError::InvalidInput(e)))
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    if !raw.is_empty() && statuses.is_empty() {
        return Err(ApiError::Core(ConductorError::InvalidInput(
            "status filter yielded no valid values — did you pass only commas?".into(),
        )));
    }

    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = WorkflowManager::new(&db);
    let runs = if let Some(ref repo_id) = params.repo {
        let repo = RepoManager::new(&db, &config).get_by_id(repo_id)?;
        mgr.list_active_workflow_runs_for_repo(&repo.id, &statuses)?
    } else {
        mgr.list_active_workflow_runs(&statuses)?
    };

    // Batch-fetch only running/waiting steps for all runs (filter pushed to SQL)
    let run_ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    let mut steps_by_run = mgr.get_active_steps_for_runs(&run_ids)?;

    // Build slug lookup maps for repo_slug / worktree_slug enrichment
    let repo_slug_map: HashMap<String, String> = RepoManager::new(&db, &config)
        .list()?
        .into_iter()
        .map(|r| (r.id, r.slug))
        .collect();
    let wt_ids: Vec<&str> = runs
        .iter()
        .filter_map(|r| r.worktree_id.as_deref())
        .collect();
    let wt_slug_map: HashMap<String, String> = WorktreeManager::new(&db, &config)
        .get_by_ids(&wt_ids)?
        .into_iter()
        .map(|wt| (wt.id, wt.slug))
        .collect();

    // ── Time estimation: batch-fetch historical durations, step histories, and LLM estimates ──
    let active_run_ids: Vec<&str> = runs
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                WorkflowRunStatus::Running | WorkflowRunStatus::Pending
            )
        })
        .map(|r| r.id.as_str())
        .collect();
    let plan_estimates = mgr.get_plan_estimates_for_runs(&active_run_ids)?;

    // Collect unique workflow names for active runs
    let active_workflow_names: std::collections::HashSet<&str> = runs
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                WorkflowRunStatus::Running | WorkflowRunStatus::Pending
            )
        })
        .map(|r| r.workflow_name.as_str())
        .collect();

    // Batch-fetch historical durations (workflow-level) and step histories
    let historical_durations: HashMap<String, Vec<i64>> = active_workflow_names
        .iter()
        .filter_map(|name| match mgr.get_completed_run_durations(name, 15) {
            Ok(d) => Some((name.to_string(), d)),
            Err(e) => {
                tracing::warn!(workflow = %name, "get_completed_run_durations failed: {e}");
                None
            }
        })
        .collect();
    let step_histories: HashMap<String, HashMap<String, Vec<i64>>> = active_workflow_names
        .into_iter()
        .filter_map(|name| match mgr.get_completed_step_durations(name, 20) {
            Ok(d) => Some((name.to_string(), d)),
            Err(e) => {
                tracing::warn!(workflow = %name, "get_completed_step_durations failed: {e}");
                None
            }
        })
        .collect();

    // Batch-fetch all steps for active runs (for live remaining estimation)
    let mut all_active_run_steps: HashMap<String, Vec<WorkflowRunStep>> =
        mgr.get_steps_for_runs(&active_run_ids)?;
    let responses: Vec<WorkflowRunResponse> = runs
        .into_iter()
        .map(|run| {
            let active_steps = steps_by_run.remove(&run.id).unwrap_or_default();
            let repo_slug = run
                .repo_id
                .as_deref()
                .and_then(|id| repo_slug_map.get(id))
                .cloned();
            let worktree_slug = run
                .worktree_id
                .as_deref()
                .and_then(|id| wt_slug_map.get(id))
                .cloned();

            // Determine the "current" step — get first running step
            let current = active_steps
                .iter()
                .find(|s| matches!(s.status, WorkflowStepStatus::Running));
            let current_step = current.map(|s| s.position + 1); // 1-indexed
            let current_step_name = current.map(|s| s.step_name.clone());
            let current_iteration = current.map(|s| s.iteration);

            // Compute total_steps and max_iterations from definition_snapshot
            let def: Option<WorkflowDef> = run
                .definition_snapshot
                .as_deref()
                .and_then(|snap| serde_json::from_str(snap).ok());
            let total_steps = def.as_ref().map(|d| d.total_nodes());
            let max_iterations = current_step_name
                .as_deref()
                .and_then(|name| def.as_ref().and_then(|d| d.max_iterations_for_step(name)))
                .map(|v| v as i64);

            // Compute time estimates for active runs
            let is_active = matches!(
                run.status,
                WorkflowRunStatus::Running | WorkflowRunStatus::Pending
            );

            let (
                estimated_duration_ms,
                estimated_remaining_ms,
                estimate_confidence,
                estimated_remaining_low_ms,
                estimated_remaining_high_ms,
                step_estimates_out,
            ) = if is_active {
                let llm_est = plan_estimates.get(&run.id).copied();
                let hist = historical_durations
                    .get(&run.workflow_name)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);

                // Try per-step live estimation first
                let sh = step_histories.get(&run.workflow_name);
                let run_steps = all_active_run_steps.remove(&run.id).unwrap_or_default();
                let step_ests = sh.map(estimation::estimate_all_steps);
                let live = step_ests
                    .as_ref()
                    .and_then(|se| estimation::live_remaining_estimate(&run_steps, se));

                if let Some(ref live_est) = live {
                    // Use per-step live estimate
                    let wf_est = estimation::estimate_with_confidence(llm_est, hist);
                    (
                        wf_est.as_ref().map(|e| e.point_ms),
                        Some(live_est.remaining_ms),
                        Some(live_est.confidence),
                        Some(live_est.low_remaining_ms),
                        Some(live_est.high_remaining_ms),
                        step_ests,
                    )
                } else {
                    // Fall back to workflow-level estimate with confidence
                    let wf_est = estimation::estimate_with_confidence(llm_est, hist);
                    match wf_est {
                        Some(ref est) => {
                            let remaining =
                                estimation::estimated_remaining_ms(est.point_ms, &run.started_at);
                            let remaining_low =
                                estimation::estimated_remaining_ms(est.low_ms, &run.started_at);
                            let remaining_high =
                                estimation::estimated_remaining_ms(est.high_ms, &run.started_at);
                            (
                                Some(est.point_ms),
                                Some(remaining),
                                Some(est.confidence),
                                Some(remaining_low),
                                Some(remaining_high),
                                None,
                            )
                        }
                        None => (None, None, None, None, None, None),
                    }
                }
            } else {
                (None, None, None, None, None, None)
            };
            WorkflowRunResponse {
                run,
                active_steps,
                repo_slug,
                worktree_slug,
                total_steps,
                current_step,
                current_step_name,
                current_iteration,
                max_iterations,
                estimated_duration_ms,
                estimated_remaining_ms,
                estimate_confidence,
                estimated_remaining_low_ms,
                estimated_remaining_high_ms,
                step_estimates: step_estimates_out,
            }
        })
        .collect();

    Ok(Json(responses))
}

#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/workflows/runs",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    responses(
        (status = 200, description = "List of workflow runs for worktree", body = Vec<WorkflowRun>),
    ),
    tag = "workflows",
)]
/// GET /api/worktrees/{id}/workflows/runs
pub async fn list_workflow_runs(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<Vec<WorkflowRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let runs = mgr.list_workflow_runs(&worktree_id)?;
    Ok(Json(runs))
}

#[utoipa::path(
    get,
    path = "/api/workflows/runs/{id}",
    params(
        ("id" = String, Path, description = "Workflow run ID"),
    ),
    responses(
        (status = 200, description = "Workflow run", body = WorkflowRun),
        (status = 404, description = "Workflow run not found"),
    ),
    tag = "workflows",
)]
/// GET /api/workflows/runs/{id}
pub async fn get_workflow_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<WorkflowRun>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let run = mgr
        .get_workflow_run(&id)?
        .ok_or_else(|| ApiError::Core(ConductorError::WorkflowRunNotFound { id: id.clone() }))?;
    Ok(Json(run))
}

#[utoipa::path(
    get,
    path = "/api/workflows/runs/{id}/steps",
    params(
        ("id" = String, Path, description = "Workflow run ID"),
    ),
    responses(
        (status = 200, description = "List of workflow run steps", body = Vec<WorkflowRunStep>),
        (status = 404, description = "Workflow run not found"),
    ),
    tag = "workflows",
)]
/// GET /api/workflows/runs/{id}/steps
pub async fn get_workflow_steps(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<WorkflowRunStep>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let steps = mgr.get_workflow_steps(&id)?;
    Ok(Json(steps))
}

#[utoipa::path(
    get,
    path = "/api/workflows/runs/{id}/steps/{step_id}/fan_out_items",
    params(
        ("id" = String, Path, description = "Workflow run ID"),
        ("step_id" = String, Path, description = "Workflow run step ID"),
    ),
    responses(
        (status = 200, description = "Fan-out items for the step", body = Vec<FanOutItemRow>),
    ),
    tag = "workflows",
)]
/// GET /api/workflows/runs/{id}/steps/{step_id}/fan_out_items
pub async fn get_fan_out_items(
    State(state): State<AppState>,
    Path((id, step_id)): Path<(String, String)>,
) -> Result<Json<Vec<FanOutItemRow>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let items = mgr.get_fan_out_items_checked(&id, &step_id, None)?;
    Ok(Json(items))
}

/// GET /api/workflows/analytics/aggregates?repo_id=
#[derive(Deserialize, utoipa::IntoParams)]
pub struct AggregatesQuery {
    pub repo_id: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/workflows/analytics/aggregates",
    params(AggregatesQuery),
    responses(
        (status = 200, description = "Token aggregates per workflow", body = Vec<WorkflowTokenAggregate>),
    ),
    tag = "workflows",
)]
pub async fn get_token_aggregates(
    State(state): State<AppState>,
    Query(q): Query<AggregatesQuery>,
) -> Result<Json<Vec<WorkflowTokenAggregate>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let rows = mgr.get_workflow_token_aggregates(q.repo_id.as_deref())?;
    Ok(Json(rows))
}

/// GET /api/workflows/analytics/trend?workflow_name=&granularity=daily|weekly
#[derive(Deserialize, utoipa::IntoParams)]
pub struct TrendQuery {
    pub workflow_name: String,
    pub granularity: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/workflows/analytics/trend",
    params(TrendQuery),
    responses(
        (status = 200, description = "Token usage trend over time", body = Vec<WorkflowTokenTrendRow>),
    ),
    tag = "workflows",
)]
pub async fn get_token_trend(
    State(state): State<AppState>,
    Query(q): Query<TrendQuery>,
) -> Result<Json<Vec<WorkflowTokenTrendRow>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let granularity = parse_granularity(q.granularity)?;
    let rows = mgr.get_workflow_token_trend(&q.workflow_name, granularity)?;
    Ok(Json(rows))
}

/// GET /api/workflows/analytics/heatmap?workflow_name=&runs=20
#[derive(Deserialize, utoipa::IntoParams)]
pub struct HeatmapQuery {
    pub workflow_name: String,
    pub runs: Option<usize>,
}

#[utoipa::path(
    get,
    path = "/api/workflows/analytics/heatmap",
    params(HeatmapQuery),
    responses(
        (status = 200, description = "Step token usage heatmap", body = Vec<StepTokenHeatmapRow>),
    ),
    tag = "workflows",
)]
pub async fn get_step_heatmap(
    State(state): State<AppState>,
    Query(q): Query<HeatmapQuery>,
) -> Result<Json<Vec<StepTokenHeatmapRow>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let limit = q.runs.unwrap_or(20);
    let rows = mgr.get_step_token_heatmap(&q.workflow_name, limit)?;
    Ok(Json(rows))
}

/// GET /api/workflows/analytics/runs?workflow_name=&days=30
#[derive(Deserialize, utoipa::IntoParams)]
pub struct RunMetricsQuery {
    pub workflow_name: String,
    pub days: Option<u32>,
}

#[utoipa::path(
    get,
    path = "/api/workflows/analytics/runs",
    params(RunMetricsQuery),
    responses(
        (status = 200, description = "Workflow run metrics", body = Vec<WorkflowRunMetricsRow>),
    ),
    tag = "workflows",
)]
pub async fn get_run_metrics(
    State(state): State<AppState>,
    Query(q): Query<RunMetricsQuery>,
) -> Result<Json<Vec<WorkflowRunMetricsRow>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let days = q.days.unwrap_or(30);
    let rows = mgr.get_run_metrics(&q.workflow_name, days)?;
    Ok(Json(rows))
}

/// GET /api/workflows/analytics/failure-trend?workflow_name=&granularity=daily|weekly
#[utoipa::path(
    get,
    path = "/api/workflows/analytics/failure-trend",
    params(TrendQuery),
    responses(
        (status = 200, description = "Workflow failure rate trend", body = Vec<WorkflowFailureRateTrendRow>),
    ),
    tag = "workflows",
)]
pub async fn get_failure_trend(
    State(state): State<AppState>,
    Query(q): Query<TrendQuery>,
) -> Result<Json<Vec<WorkflowFailureRateTrendRow>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let granularity = parse_granularity(q.granularity)?;
    let rows = mgr.get_workflow_failure_rate_trend(&q.workflow_name, granularity)?;
    Ok(Json(rows))
}

#[utoipa::path(
    get,
    path = "/api/workflows/analytics/failure-heatmap",
    params(HeatmapQuery),
    responses(
        (status = 200, description = "Step failure heatmap", body = Vec<StepFailureHeatmapRow>),
    ),
    tag = "workflows",
)]
/// GET /api/workflows/analytics/failure-heatmap?workflow_name=&runs=20
pub async fn get_failure_heatmap(
    State(state): State<AppState>,
    Query(q): Query<HeatmapQuery>,
) -> Result<Json<Vec<StepFailureHeatmapRow>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let limit = q.runs.unwrap_or(20);
    let rows = mgr.get_step_failure_heatmap(&q.workflow_name, limit)?;
    Ok(Json(rows))
}

#[utoipa::path(
    get,
    path = "/api/workflows/analytics/step-retries",
    params(HeatmapQuery),
    responses(
        (status = 200, description = "Step retry analytics", body = Vec<StepRetryAnalyticsRow>),
    ),
    tag = "workflows",
)]
/// GET /api/workflows/analytics/step-retries?workflow_name=&runs=20
pub async fn get_step_retry_analytics(
    State(state): State<AppState>,
    Query(q): Query<HeatmapQuery>,
) -> Result<Json<Vec<StepRetryAnalyticsRow>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let limit = q.runs.unwrap_or(20);
    let rows = mgr.get_step_retry_analytics(&q.workflow_name, limit)?;
    Ok(Json(rows))
}

/// GET /api/workflows/analytics/percentiles?workflow_name=&days=30
#[derive(Deserialize, utoipa::IntoParams)]
pub struct PercentilesQuery {
    pub workflow_name: String,
    pub days: Option<u32>,
}

#[utoipa::path(
    get,
    path = "/api/workflows/analytics/percentiles",
    params(PercentilesQuery),
    responses(
        (status = 200, description = "Workflow percentile statistics"),
    ),
    tag = "workflows",
)]
pub async fn get_workflow_percentiles(
    State(state): State<AppState>,
    Query(q): Query<PercentilesQuery>,
) -> Result<Json<Option<WorkflowPercentiles>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let days = q.days.unwrap_or(30);
    let result = mgr.get_workflow_percentiles(&q.workflow_name, days)?;
    Ok(Json(result))
}

/// GET /api/workflows/analytics/regressions?recent_days=7&baseline_days=30&min_runs=5
#[derive(Deserialize, utoipa::IntoParams)]
pub struct RegressionsQuery {
    pub recent_days: Option<i64>,
    pub baseline_days: Option<i64>,
    pub min_runs: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/api/workflows/analytics/regressions",
    params(RegressionsQuery),
    responses(
        (status = 200, description = "Workflow regression signals", body = Vec<WorkflowRegressionSignal>),
    ),
    tag = "workflows",
)]
pub async fn get_workflow_regressions(
    State(state): State<AppState>,
    Query(q): Query<RegressionsQuery>,
) -> Result<Json<Vec<WorkflowRegressionSignal>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let recent_days = q.recent_days.unwrap_or(7);
    let baseline_days = q.baseline_days.unwrap_or(30);
    let min_runs = q.min_runs.unwrap_or(REGRESSION_MIN_RECENT_RUNS);
    let signals = mgr.get_workflow_regression_signals(min_runs, recent_days, baseline_days)?;
    Ok(Json(signals))
}

/// GET /api/workflows/analytics/gates?workflow_name=&days=30
#[derive(Deserialize, utoipa::IntoParams)]
pub struct GateAnalyticsQuery {
    pub workflow_name: String,
    pub days: Option<u32>,
}

#[utoipa::path(
    get,
    path = "/api/workflows/analytics/gates",
    params(GateAnalyticsQuery),
    responses(
        (status = 200, description = "Gate analytics", body = Vec<GateAnalyticsRow>),
    ),
    tag = "workflows",
)]
pub async fn get_gate_analytics(
    State(state): State<AppState>,
    Query(q): Query<GateAnalyticsQuery>,
) -> Result<Json<Vec<GateAnalyticsRow>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let days = q.days.unwrap_or(30);
    let rows = mgr.get_gate_analytics(&q.workflow_name, days)?;
    Ok(Json(rows))
}

#[utoipa::path(
    get,
    path = "/api/workflows/analytics/gates/pending",
    responses(
        (status = 200, description = "List of pending gates across all workflow runs", body = Vec<PendingGateAnalyticsRow>),
    ),
    tag = "workflows",
)]
/// GET /api/workflows/analytics/gates/pending
pub async fn get_pending_gates(
    State(state): State<AppState>,
) -> Result<Json<Vec<PendingGateAnalyticsRow>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let rows = mgr.get_all_pending_gates()?;
    Ok(Json(rows))
}

#[utoipa::path(
    get,
    path = "/api/workflows/runs/{id}/steps/{step_name}/log",
    params(
        ("id" = String, Path, description = "Workflow run ID"),
        ("step_name" = String, Path, description = "Workflow step name"),
    ),
    responses(
        (status = 200, description = "Step log content"),
        (status = 404, description = "Run or step not found"),
    ),
    tag = "workflows",
)]
/// GET /api/workflows/runs/{id}/steps/{step_name}/log
pub async fn get_workflow_step_log(
    State(state): State<AppState>,
    Path((run_id, step_name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use conductor_core::agent::AgentManager;

    // Hold the DB lock only for the DB queries, then drop it before the file read.
    let log_path = {
        let db = state.db.lock().await;
        let wf_mgr = WorkflowManager::new(&db);

        // Verify run exists
        wf_mgr.get_workflow_run(&run_id)?.ok_or_else(|| {
            ApiError::Core(ConductorError::WorkflowRunNotFound { id: run_id.clone() })
        })?;

        // Find matching step — last iteration wins
        let steps = wf_mgr.get_workflow_steps(&run_id)?;
        let step = steps
            .into_iter()
            .filter(|s| s.step_name == step_name)
            .max_by_key(|s| s.iteration)
            .ok_or_else(|| {
                ApiError::Core(ConductorError::Workflow(format!(
                    "step '{step_name}' not found in run '{run_id}'"
                )))
            })?;

        // Gate/skipped steps have no child_run_id
        let child_run_id = step.child_run_id.ok_or_else(|| {
            ApiError::Core(ConductorError::Workflow(format!(
                "step '{step_name}' has no agent run (gate or skipped step)"
            )))
        })?;

        // Resolve log path from agent run, fall back to default path
        let agent_mgr = AgentManager::new(&db);
        agent_mgr
            .get_run(&child_run_id)?
            .and_then(|r| r.log_file)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| conductor_core::config::agent_log_path(&child_run_id))
    }; // DB lock released here

    // Non-blocking async file read — does not block the tokio worker thread
    let log = tokio::fs::read_to_string(&log_path).await.map_err(|e| {
        ApiError::Core(ConductorError::Workflow(format!(
            "failed to read log file '{}' for run '{run_id}' step '{step_name}': {e}",
            log_path.display()
        )))
    })?;

    Ok(Json(serde_json::json!({ "log": log })))
}

#[utoipa::path(
    get,
    path = "/api/workflows/runs/{id}/children",
    params(
        ("id" = String, Path, description = "Workflow run ID"),
    ),
    responses(
        (status = 200, description = "List of child workflow runs", body = Vec<WorkflowRun>),
    ),
    tag = "workflows",
)]
/// GET /api/workflows/runs/{id}/children
pub async fn get_child_workflow_runs(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<WorkflowRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let children = mgr.list_child_workflow_runs(&id)?;
    Ok(Json(children))
}

#[utoipa::path(
    post,
    path = "/api/workflows/runs/{id}/cancel",
    params(
        ("id" = String, Path, description = "Workflow run ID"),
    ),
    responses(
        (status = 200, description = "Workflow cancelled"),
        (status = 404, description = "Workflow run not found"),
    ),
    tag = "workflows",
)]
/// POST /api/workflows/runs/{id}/cancel
pub async fn cancel_workflow(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);

    // Verify run exists
    let run = mgr
        .get_workflow_run(&id)?
        .ok_or_else(|| ApiError::Core(ConductorError::WorkflowRunNotFound { id: id.clone() }))?;

    mgr.cancel_run(&id, "Cancelled by user")?;

    state.events.emit(ConductorEvent::WorkflowRunStatusChanged {
        run_id: id.clone(),
        worktree_id: run.worktree_id.clone(),
        status: "cancelled".to_string(),
    });

    Ok(Json(
        serde_json::json!({ "status": "cancelled", "run_id": id }),
    ))
}

#[utoipa::path(
    post,
    path = "/api/workflows/runs/{id}/resume",
    params(
        ("id" = String, Path, description = "Workflow run ID"),
    ),
    request_body(content = ResumeWorkflowRequest, description = "Resume parameters"),
    responses(
        (status = 202, description = "Workflow resume started"),
        (status = 404, description = "Workflow run not found"),
    ),
    tag = "workflows",
)]
/// POST /api/workflows/runs/{id}/resume
pub async fn resume_workflow_endpoint(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ResumeWorkflowRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let config = state.config.read().await.clone();
    let model = req.model.clone();
    let from_step = req.from_step.clone();
    let restart = req.restart.unwrap_or(false);

    // Validate the run exists and is in a resumable state before spawning.
    // Also capture the workflow name and target label for the completion notification.
    let (workflow_name, target_label, run_repo_id, run_worktree_id) = {
        let db = state.db.lock().await;
        let mgr = WorkflowManager::new(&db);
        let run = mgr.get_workflow_run(&id)?.ok_or_else(|| {
            ApiError::Core(ConductorError::WorkflowRunNotFound { id: id.clone() })
        })?;
        validate_resume_preconditions(&run.status, restart, from_step.as_deref())
            .map_err(ApiError::Core)?;
        (
            run.workflow_name.clone(),
            run.target_label.clone(),
            run.repo_id.clone(),
            run.worktree_id.clone(),
        )
    }; // DB lock released here

    // Spawn blocking task with its own DB connection (same pattern as run_workflow)
    let state_clone = state.clone();
    let run_id = id.clone();
    let notifications = config.notifications.clone();
    let notify_hooks = config.notify.hooks.clone();
    let db_path = state.db_path.clone();
    tokio::task::spawn_blocking(move || {
        let params = WorkflowResumeStandalone {
            config,
            workflow_run_id: run_id,
            model,
            from_step,
            restart,
            db_path: None,
            conductor_bin_dir: conductor_core::workflow::resolve_conductor_bin_dir(),
        };

        let result = conductor_core::workflow::resume_workflow_standalone(&params);

        let conn = match conductor_core::db::open_database(&db_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("notify: DB open failed: {e}");
                return;
            }
        };

        let (resume_repo_slug, resume_branch) =
            conductor_core::notify::parse_target_label(target_label.as_deref());

        match result {
            Ok(res) => {
                let succeeded = res.all_succeeded;
                let status = if succeeded { "completed" } else { "failed" };

                notify_workflow(
                    &conn,
                    &notifications,
                    &notify_hooks,
                    &WorkflowNotificationArgs {
                        run_id: &res.workflow_run_id,
                        workflow_name: &workflow_name,
                        target_label: target_label.as_deref(),
                        succeeded,
                        parent_workflow_run_id: None, // workflows resumed from web are always root runs
                        repo_slug: resume_repo_slug,
                        branch: resume_branch,
                        duration_ms: None,
                        ticket_url: None,
                        error: None,
                        repo_id: run_repo_id.as_deref(),
                        worktree_id: run_worktree_id.as_deref(),
                    },
                );

                state_clone
                    .events
                    .emit(ConductorEvent::WorkflowRunStatusChanged {
                        run_id: res.workflow_run_id,
                        worktree_id: res.worktree_id,
                        status: status.to_string(),
                    });
            }
            Err(e) => {
                tracing::error!("Workflow resume failed: {e}");
                notify_workflow(
                    &conn,
                    &notifications,
                    &notify_hooks,
                    &WorkflowNotificationArgs {
                        run_id: &params.workflow_run_id,
                        workflow_name: &workflow_name,
                        target_label: target_label.as_deref(),
                        succeeded: false,
                        parent_workflow_run_id: None, // workflows resumed from web are always root runs
                        repo_slug: resume_repo_slug,
                        branch: resume_branch,
                        duration_ms: None,
                        ticket_url: None,
                        error: None,
                        repo_id: run_repo_id.as_deref(),
                        worktree_id: run_worktree_id.as_deref(),
                    },
                );
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "resuming",
            "run_id": id,
        })),
    ))
}

#[utoipa::path(
    post,
    path = "/api/workflows/runs/{id}/gate/approve",
    params(
        ("id" = String, Path, description = "Workflow run ID"),
    ),
    request_body(content = GateActionRequest, description = "Gate approval details"),
    responses(
        (status = 200, description = "Gate approved"),
        (status = 404, description = "Workflow run or waiting gate not found"),
    ),
    tag = "workflows",
)]
/// POST /api/workflows/runs/{id}/gate/approve
pub async fn approve_gate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<GateActionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);

    let step = mgr.find_waiting_gate(&id)?.ok_or_else(|| {
        ApiError::Core(ConductorError::Workflow(
            "No waiting gate found for this workflow run".to_string(),
        ))
    })?;

    mgr.approve_gate(
        &step.id,
        "user",
        req.feedback.as_deref(),
        req.selections.as_deref(),
    )?;

    state
        .events
        .emit(ConductorEvent::WorkflowStepStatusChanged {
            run_id: id.clone(),
            step_id: step.id.clone(),
            status: "completed".to_string(),
        });

    Ok(Json(serde_json::json!({
        "status": "approved",
        "step_id": step.id,
    })))
}

#[utoipa::path(
    post,
    path = "/api/workflows/runs/{id}/gate/reject",
    params(
        ("id" = String, Path, description = "Workflow run ID"),
    ),
    responses(
        (status = 200, description = "Gate rejected"),
        (status = 404, description = "Workflow run or waiting gate not found"),
    ),
    tag = "workflows",
)]
/// POST /api/workflows/runs/{id}/gate/reject
pub async fn reject_gate(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);

    let step = mgr.find_waiting_gate(&id)?.ok_or_else(|| {
        ApiError::Core(ConductorError::Workflow(
            "No waiting gate found for this workflow run".to_string(),
        ))
    })?;

    mgr.reject_gate(&step.id, "user", None)?;

    state
        .events
        .emit(ConductorEvent::WorkflowStepStatusChanged {
            run_id: id.clone(),
            step_id: step.id.clone(),
            status: "failed".to_string(),
        });

    Ok(Json(serde_json::json!({
        "status": "rejected",
        "step_id": step.id,
    })))
}

// ── Template endpoints ────────────────────────────────────────────────

/// GET /api/templates — list all embedded workflow templates.
#[utoipa::path(
    get,
    path = "/api/templates",
    responses(
        (status = 200, description = "List of embedded workflow templates"),
    ),
    tag = "workflows",
)]
pub async fn list_templates() -> Json<Vec<conductor_core::workflow_template::TemplateFrontmatter>> {
    use conductor_core::workflow_template::list_embedded_templates;

    let templates = list_embedded_templates();
    Json(templates.into_iter().map(|t| t.metadata).collect())
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct InstantiateTemplateRequest {
    pub template: String,
    pub repo: String,
    pub worktree: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct InstantiateTemplateResponse {
    pub template_name: String,
    pub template_version: String,
    pub suggested_filename: String,
    pub prompt: String,
}

#[utoipa::path(
    post,
    path = "/api/templates/instantiate",
    request_body(content = InstantiateTemplateRequest, description = "Template instantiation parameters"),
    responses(
        (status = 200, description = "Template instantiation prompt", body = InstantiateTemplateResponse),
        (status = 404, description = "Template or repo not found"),
    ),
    tag = "workflows",
)]
/// POST /api/templates/instantiate — build the agent instantiation prompt for a template.
pub async fn instantiate_template(
    State(state): State<AppState>,
    Json(req): Json<InstantiateTemplateRequest>,
) -> Result<Json<InstantiateTemplateResponse>, ApiError> {
    use conductor_core::workflow_template::{
        build_instantiation_prompt, collect_existing_workflow_names, get_embedded_template,
    };

    let tmpl = get_embedded_template(&req.template).ok_or_else(|| {
        ApiError::Core(ConductorError::InvalidInput(format!(
            "Template '{}' not found",
            req.template
        )))
    })?;

    let db = state.db.lock().await;
    let config = state.config.read().await;
    let repo = RepoManager::new(&db, &config).get_by_slug(&req.repo)?;

    let working_dir = if let Some(ref wt_slug) = req.worktree {
        let wt_mgr = WorktreeManager::new(&db, &config);
        let wt = wt_mgr.get_by_slug_or_branch(&repo.id, wt_slug)?;
        wt.path
    } else {
        repo.local_path.clone()
    };

    let existing_names = collect_existing_workflow_names(&working_dir, &repo.local_path);

    let prompt_result = build_instantiation_prompt(&tmpl, &working_dir, &existing_names);

    Ok(Json(InstantiateTemplateResponse {
        template_name: tmpl.metadata.name,
        template_version: tmpl.metadata.version,
        suggested_filename: prompt_result.suggested_filename,
        prompt: prompt_result.prompt,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use conductor_core::workflow::WorkflowStepStatus;
    use tokio::sync::{Mutex, RwLock};
    use tower::ServiceExt;

    use crate::events::EventBus;
    use crate::routes::api_router;
    use crate::test_helpers as th;

    // Workflow tests never exercise the worktree create/delete spawn_blocking
    // paths, so db_path does not need to point to a live file. These wrappers
    // drop the NamedTempFile immediately, which is safe here.
    fn empty_state() -> AppState {
        th::empty_state().0
    }
    fn seeded_state() -> AppState {
        th::seeded_state().0
    }
    fn seeded_state_with_agent_run() -> AppState {
        th::seeded_state_with_agent_run().0
    }

    async fn get_response(uri: &str, state: AppState) -> (StatusCode, serde_json::Value) {
        let app = api_router().with_state(state);
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    fn assert_error_contains(body: &serde_json::Value, substr: &str) {
        assert!(
            body["error"].as_str().unwrap_or("").contains(substr),
            "unexpected error body: {body}"
        );
    }

    #[tokio::test]
    async fn status_valid_returns_200() {
        let (status, _) = get_response("/api/workflows/runs?status=running", empty_state()).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn status_bogus_returns_400() {
        let (status, body) = get_response("/api/workflows/runs?status=bogus", empty_state()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error_contains(&body, "unknown WorkflowRunStatus: bogus");
    }

    #[tokio::test]
    async fn status_mixed_valid_and_bogus_returns_400() {
        let (status, _) =
            get_response("/api/workflows/runs?status=running,bogus", empty_state()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn status_empty_param_returns_200() {
        let (status, _) = get_response("/api/workflows/runs?status=", empty_state()).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn status_absent_returns_200() {
        let (status, _) = get_response("/api/workflows/runs", empty_state()).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn status_trailing_comma_returns_400() {
        let (status, _) = get_response("/api/workflows/runs?status=running,", empty_state()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn status_leading_comma_returns_400() {
        let (status, _) = get_response("/api/workflows/runs?status=,running", empty_state()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn status_double_comma_returns_400() {
        let (status, _) =
            get_response("/api/workflows/runs?status=running,,waiting", empty_state()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn status_all_commas_returns_400() {
        let (status, body) = get_response("/api/workflows/runs?status=,,,", empty_state()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error_contains(&body, "empty status token in list");
    }

    #[tokio::test]
    async fn status_whitespace_only_param_returns_400() {
        let (status, body) = get_response("/api/workflows/runs?status=%20", empty_state()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error_contains(&body, "empty status token in list");
    }

    #[tokio::test]
    async fn status_whitespace_only_token_in_csv_returns_400() {
        let (status, body) = get_response(
            "/api/workflows/runs?status=running,%20,waiting",
            empty_state(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error_contains(&body, "empty status token in list");
    }

    #[tokio::test]
    async fn status_whitespace_only_tokens_only_returns_400() {
        let (status, _) = get_response("/api/workflows/runs?status=%20,%20", empty_state()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn repo_filter_by_ulid_returns_matching_run() {
        let state = empty_state();
        let repo_id = "01TESTREPOULID0000000000001";
        {
            let db = state.db.lock().await;
            conductor_core::test_helpers::insert_test_repo(&db, repo_id, "test-repo", "/tmp/repo");
            conductor_core::test_helpers::insert_test_worktree(
                &db,
                "wt1",
                repo_id,
                "feat-test",
                "/tmp/ws/feat-test",
            );
            conductor_core::test_helpers::insert_test_agent_run(&db, "ar1", "wt1");

            let mgr = WorkflowManager::new(&db);
            let run = mgr
                .create_workflow_run("test-wf", Some("wt1"), "ar1", false, "manual", None)
                .unwrap();
            mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
                .unwrap();
        }

        // Filtering by ULID returns the run
        let (status, body) =
            get_response(&format!("/api/workflows/runs?repo={repo_id}"), state).await;
        assert_eq!(status, StatusCode::OK);
        let runs = body.as_array().unwrap();
        assert_eq!(runs.len(), 1, "should return exactly one run for the repo");
    }

    #[tokio::test]
    async fn repo_filter_nonexistent_ulid_returns_404() {
        let (status, _) = get_response(
            "/api/workflows/runs?repo=01NONEXISTENTREPO000000000",
            empty_state(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn active_steps_attached_filters_to_running_and_waiting() {
        let state = seeded_state_with_agent_run();
        {
            let db = state.db.lock().await;

            let mgr = WorkflowManager::new(&db);
            // worktree_id = None so the run is visible without an active worktree join
            let run = mgr
                .create_workflow_run("test-wf", None, "ar1", false, "manual", None)
                .unwrap();
            mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None, None)
                .unwrap();

            // Insert 4 steps with mixed statuses
            let id_a = mgr
                .insert_step(&run.id, "step-running", "actor", false, 0, 0)
                .unwrap();
            mgr.update_step_status(
                &id_a,
                WorkflowStepStatus::Running,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let id_b = mgr
                .insert_step(&run.id, "step-waiting", "actor", false, 1, 0)
                .unwrap();
            mgr.update_step_status(
                &id_b,
                WorkflowStepStatus::Waiting,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let id_c = mgr
                .insert_step(&run.id, "step-completed", "actor", false, 2, 0)
                .unwrap();
            mgr.update_step_status(
                &id_c,
                WorkflowStepStatus::Completed,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let id_d = mgr
                .insert_step(&run.id, "step-failed", "actor", false, 3, 0)
                .unwrap();
            mgr.update_step_status(
                &id_d,
                WorkflowStepStatus::Failed,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        }

        let (status, body) = get_response("/api/workflows/runs", state).await;
        assert_eq!(status, StatusCode::OK);

        let runs = body.as_array().unwrap();
        assert_eq!(runs.len(), 1);

        let active_steps = runs[0]["active_steps"].as_array().unwrap();
        assert_eq!(
            active_steps.len(),
            2,
            "only running and waiting steps should be attached"
        );

        let step_names: Vec<&str> = active_steps
            .iter()
            .map(|s| s["step_name"].as_str().unwrap())
            .collect();
        assert!(
            step_names.contains(&"step-running"),
            "running step should be included"
        );
        assert!(
            step_names.contains(&"step-waiting"),
            "waiting step should be included"
        );
    }

    #[tokio::test]
    async fn notify_workflow_completes_without_panic() {
        let conn = conductor_core::test_helpers::create_test_conn();
        let notifications = conductor_core::config::NotificationConfig::default(); // enabled=false

        tokio::task::spawn_blocking(move || {
            notify_workflow(
                &conn,
                &notifications,
                &[],
                &WorkflowNotificationArgs {
                    run_id: "test-run-id",
                    workflow_name: "test-wf",
                    target_label: None,
                    succeeded: false,
                    parent_workflow_run_id: None,
                    repo_slug: "",
                    branch: "",
                    duration_ms: None,
                    ticket_url: None,
                    error: None,
                    repo_id: None,
                    worktree_id: None,
                },
            );
        })
        .await
        .unwrap();
    }

    fn test_notification_config() -> conductor_core::config::NotificationConfig {
        conductor_core::config::NotificationConfig {
            enabled: true,
            workflows: Some(conductor_core::config::WorkflowNotificationConfig {
                on_success: true,
                on_failure: true,
                on_gate_human: true,
                on_gate_ci: false,
                on_gate_pr_review: true,
                on_stale: true,
            }),
            slack: conductor_core::config::SlackConfig::default(),
            web_url: None,
        }
    }

    #[tokio::test]
    async fn error_path_key_deduplicates() {
        let db = empty_state().db;

        let notifications = test_notification_config();

        let bucket = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / 60;
        let key = format!("wf-err:my-workflow:repo/wt:{bucket}");

        // First call — simulates one web process observing the failure
        let db1 = Arc::clone(&db);
        let notifications1 = notifications.clone();
        let key1 = key.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db1.blocking_lock();
            notify_workflow(
                &conn,
                &notifications1,
                &[],
                &WorkflowNotificationArgs {
                    run_id: &key1,
                    workflow_name: "my-workflow",
                    target_label: Some("repo/wt"),
                    succeeded: false,
                    parent_workflow_run_id: None,
                    repo_slug: "",
                    branch: "",
                    duration_ms: None,
                    ticket_url: None,
                    error: None,
                    repo_id: None,
                    worktree_id: None,
                },
            );
        })
        .await
        .unwrap();

        // Second call — simulates a concurrent web process observing the same failure
        let db2 = Arc::clone(&db);
        let key2 = key.clone();
        tokio::task::spawn_blocking(move || {
            let conn = db2.blocking_lock();
            notify_workflow(
                &conn,
                &notifications,
                &[],
                &WorkflowNotificationArgs {
                    run_id: &key2,
                    workflow_name: "my-workflow",
                    target_label: Some("repo/wt"),
                    succeeded: false,
                    parent_workflow_run_id: None,
                    repo_slug: "",
                    branch: "",
                    duration_ms: None,
                    ticket_url: None,
                    error: None,
                    repo_id: None,
                    worktree_id: None,
                },
            );
        })
        .await
        .unwrap();

        // Dedup: only one row should exist despite two calls with the same key
        let conn = db.lock().await;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notification_log WHERE entity_id = ?1 AND event_type = 'failed'",
                [&key],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "duplicate error-path notifications must be deduped to a single log row"
        );
    }

    #[tokio::test]
    async fn notify_workflow_with_notifications_enabled_claims_log_row() {
        let conn = conductor_core::test_helpers::create_test_conn();

        let notifications = test_notification_config();

        tokio::task::spawn_blocking(move || {
            notify_workflow(&conn, &notifications, &[], &WorkflowNotificationArgs { run_id: "run-notify-1", workflow_name: "my-workflow", target_label: None, succeeded: true, parent_workflow_run_id: None, repo_slug: "", branch: "", duration_ms: None, ticket_url: None, error: None, repo_id: None, worktree_id: None });

            // Verify the dedup row was inserted into notification_log
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM notification_log WHERE entity_id = ?1 AND event_type = ?2",
                    rusqlite::params!["run-notify-1", "completed"],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                count, 1,
                "notification_log must contain exactly one dedup row"
            );
        })
        .await
        .unwrap();
    }

    /// When execute_workflow creates the run record before failing, the slot holds
    /// the real ULID. resolve_error_run_id must return that ULID so dedup aligns
    /// with any concurrent TUI notification for the same run.
    #[test]
    fn resolve_error_run_id_uses_real_run_id_when_slot_populated() {
        let real_run_id = "01REAL0000000000000000000X".to_string();
        let slot: RunIdSlot = std::sync::Arc::new((
            std::sync::Mutex::new(Some(real_run_id.clone())),
            std::sync::Condvar::new(),
        ));
        assert_eq!(
            resolve_error_run_id(&slot, "my-workflow", "repo/wt"),
            real_run_id,
            "must return the real run ULID from the slot when populated"
        );
    }

    /// When the failure happens before the run record is created, the slot is empty.
    /// resolve_error_run_id must fall back to the deterministic bucket key.
    #[test]
    fn resolve_error_run_id_uses_bucket_key_when_slot_empty() {
        let slot: RunIdSlot =
            std::sync::Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));
        let key = resolve_error_run_id(&slot, "my-workflow", "repo/wt");
        assert!(
            key.starts_with("wf-err:"),
            "must fall back to bucket key when slot is empty, got: {key}"
        );
        assert!(
            key.contains("my-workflow"),
            "bucket key must embed workflow name, got: {key}"
        );
        assert!(
            key.contains("repo/wt"),
            "bucket key must embed label, got: {key}"
        );
    }

    /// Exercises the actual run_workflow handler through the HTTP layer to verify
    /// the end-to-end wiring: the handler must return 202 Accepted, the
    /// background task must complete (signalled via workflow_done_notify),
    /// and a workflow_runs row must be created in the database.
    #[tokio::test]
    async fn run_workflow_handler_calls_execute_workflow() {
        // Create a temp dir with a minimal no-op workflow file.
        let tmp = tempfile::TempDir::new().unwrap();
        let wf_dir = tmp.path().join(".conductor").join("workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("noop.wf"),
            "workflow noop { meta { description = \"no-op\" targets = [\"worktree\"] } }",
        )
        .unwrap();
        let wt_path = tmp.path().to_str().unwrap().to_string();

        // Create a temporary file-based database so workflow execution can access the same DB
        let test_db_path = tmp.path().join("test.db");
        std::env::set_var("CONDUCTOR_DB_PATH", &test_db_path);

        // Create a test database connection and apply migrations
        let conn = conductor_core::db::open_database(&test_db_path).unwrap();

        let notify = Arc::new(tokio::sync::Notify::new());
        let state = AppState {
            db: Arc::new(Mutex::new(conn)),
            config: Arc::new(RwLock::new(conductor_core::config::Config::default())),
            events: EventBus::new(1),
            db_path: test_db_path.clone(),
            workflow_done_notify: Some(Arc::clone(&notify)),
        };
        {
            let db = state.db.lock().await;
            conductor_core::test_helpers::insert_test_repo(&db, "r1", "test-repo", &wt_path);
            conductor_core::test_helpers::insert_test_worktree(
                &db,
                "w1",
                "r1",
                "feat-test",
                &wt_path,
            );
        }

        let app = api_router().with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/worktrees/w1/workflows/run")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"noop"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "run_workflow must return 202 Accepted"
        );

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            !body["run_id"].is_null(),
            "run_workflow response must include a non-null run_id; got: {body}"
        );

        // Wait deterministically for the background task to signal completion.
        tokio::time::timeout(std::time::Duration::from_secs(5), notify.notified())
            .await
            .expect("run_workflow background task did not complete within 5 s");

        // Verify that a workflow_runs row was created in the database.
        {
            let db = state.db.lock().await;
            let mut stmt = db
                .prepare("SELECT COUNT(*) FROM workflow_runs WHERE workflow_name = ?")
                .unwrap();
            let count: i64 = stmt.query_row(["noop"], |row| row.get(0)).unwrap();
            assert_eq!(
                count, 1,
                "Expected exactly one workflow_runs row for 'noop' workflow"
            );
        }

        // Clean up environment variable
        std::env::remove_var("CONDUCTOR_DB_PATH");
    }

    // ── POST /api/workflows/runs tests ──────────────────────────────

    #[tokio::test]
    async fn post_workflow_run_unknown_repo_returns_error() {
        let state = empty_state();
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workflows/runs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "repo": "ghost-repo",
                            "workflow": "noop"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.status().is_client_error(),
            "unknown repo must return 4xx; got: {}",
            response.status()
        );
    }

    #[tokio::test]
    async fn post_workflow_run_unknown_worktree_returns_error() {
        let state = seeded_state();
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workflows/runs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "repo": "r1",
                            "workflow": "noop",
                            "worktree": "ghost-worktree-id"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.status().is_client_error(),
            "unknown worktree must return 4xx; got: {}",
            response.status()
        );
    }

    #[tokio::test]
    async fn post_workflow_run_unknown_workflow_returns_error() {
        let state = seeded_state();
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workflows/runs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "repo": "r1",
                            "workflow": "nonexistent-workflow-xyz"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.status().is_client_error() || response.status().is_server_error(),
            "unknown workflow must return an error; got: {}",
            response.status()
        );
    }

    #[tokio::test]
    async fn post_workflow_run_repo_only_with_valid_workflow_returns_accepted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wf_dir = tmp.path().join(".conductor").join("workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("noop.wf"),
            "workflow noop { meta { description = \"no-op\" targets = [\"worktree\"] } }",
        )
        .unwrap();
        let repo_path = tmp.path().to_str().unwrap().to_string();

        let notify = Arc::new(tokio::sync::Notify::new());
        // Hold the NamedTempFile alive so db_path remains valid for the
        // spawn_blocking closure that opens a fresh DB connection.
        let (base_state, _db_file) = th::empty_state();
        let state = AppState {
            workflow_done_notify: Some(Arc::clone(&notify)),
            ..base_state
        };
        {
            let db = state.db.lock().await;
            conductor_core::test_helpers::insert_test_repo(&db, "r1", "test-repo", &repo_path);
        }

        let app = api_router().with_state(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/workflows/runs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "repo": "r1",
                            "workflow": "noop"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "repo-only workflow run must return 202 Accepted"
        );

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            !body["run_id"].is_null(),
            "post_workflow_run response must include a non-null run_id; got: {body}"
        );

        tokio::time::timeout(std::time::Duration::from_secs(5), notify.notified())
            .await
            .expect("background task did not complete within 5 s");
    }

    // ── Template endpoint tests ──────────────────────────────────────

    #[tokio::test]
    async fn list_templates_returns_200() {
        let (status, body) = get_response("/api/templates", empty_state()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_array(), "expected array; got: {body}");
    }

    #[tokio::test]
    async fn instantiate_template_unknown_returns_error() {
        let state = empty_state();
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/templates/instantiate")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "template": "nonexistent-xyz",
                            "repo": "my-repo"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        assert!(
            status.is_client_error() || status.is_server_error(),
            "expected error status; got: {status}"
        );
    }

    #[tokio::test]
    async fn instantiate_template_unknown_repo_returns_error() {
        use conductor_core::workflow_template::list_embedded_templates;

        let templates = list_embedded_templates();
        if templates.is_empty() {
            // No embedded templates to test with — skip.
            return;
        }
        let template_name = &templates[0].metadata.name;

        let state = empty_state();
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/templates/instantiate")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "template": template_name,
                            "repo": "ghost-repo"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        assert!(
            status.is_client_error() || status.is_server_error(),
            "expected error for unknown repo; got: {status}"
        );
    }

    #[test]
    fn test_input_decl_summary_boolean_serializes_as_type() {
        use conductor_core::workflow::{InputDecl, InputType};
        let decl = InputDecl {
            name: "dry_run".to_string(),
            required: false,
            default: Some("false".to_string()),
            description: Some("Whether to do a dry run".to_string()),
            input_type: InputType::Boolean,
        };
        let summary = InputDeclSummary::from(&decl);
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["type"], "boolean");
        assert_eq!(json["defaultValue"], "false");
        assert_eq!(json["description"], "Whether to do a dry run");
        assert!(
            json.get("input_type").is_none(),
            "input_type must not appear"
        );
        assert!(json.get("default").is_none(), "default must not appear");
    }

    #[test]
    fn test_input_decl_summary_string_serializes_as_type() {
        use conductor_core::workflow::{InputDecl, InputType};
        let decl = InputDecl {
            name: "branch".to_string(),
            required: true,
            default: None,
            description: None,
            input_type: InputType::String,
        };
        let summary = InputDeclSummary::from(&decl);
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["type"], "string");
        assert!(json["defaultValue"].is_null());
        assert!(json["description"].is_null());
    }

    // --- get_workflow_step_log tests ---

    /// Seed the minimum FK chain needed for workflow run tests via AgentManager.
    /// Returns the generated run id.
    fn insert_agent_run(
        db: &rusqlite::Connection,
        worktree_id: &str,
        prompt: &str,
        status: &str,
        log_file: &str,
    ) -> String {
        use conductor_core::agent::AgentManager;
        let mgr = AgentManager::new(db);
        let run = mgr
            .create_run(Some(worktree_id), prompt, None, None)
            .expect("create agent run");
        mgr.update_run_log_file(&run.id, log_file)
            .expect("set log_file");
        if status == "completed" {
            mgr.update_run_completed(
                &run.id, None, None, None, None, None, None, None, None, None,
            )
            .expect("complete run");
        } else if status != "running" {
            panic!(
                "insert_agent_run: unsupported status {status:?}; use \"running\" or \"completed\""
            );
        }
        run.id
    }

    async fn seed_workflow_fixtures(state: &AppState) -> String {
        let db = state.db.lock().await;
        conductor_core::test_helpers::insert_test_repo(&db, "r1", "test-repo", "/tmp/repo");
        conductor_core::test_helpers::insert_test_worktree(
            &db,
            "w1",
            "r1",
            "feat-test",
            "/tmp/ws/feat-test",
        );
        conductor_core::test_helpers::insert_test_agent_run(&db, "ar1", "w1");
        let mgr = WorkflowManager::new(&db);
        mgr.create_workflow_run("test-wf", None, "ar1", false, "manual", None)
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn step_log_run_not_found_returns_error() {
        let (status, _) = get_response(
            "/api/workflows/runs/nonexistent-run/steps/my-step/log",
            empty_state(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn step_log_step_not_found_returns_error() {
        let state = empty_state();
        let run_id = seed_workflow_fixtures(&state).await;
        let (status, body) = get_response(
            &format!("/api/workflows/runs/{run_id}/steps/missing-step/log"),
            state,
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_error_contains(&body, "not found in run");
    }

    #[tokio::test]
    async fn step_log_no_child_run_returns_error() {
        let state = empty_state();
        let run_id = seed_workflow_fixtures(&state).await;
        {
            let db = state.db.lock().await;
            let mgr = WorkflowManager::new(&db);
            mgr.insert_step(&run_id, "gate-step", "actor", false, 0, 0)
                .unwrap();
            // step is left with child_run_id = NULL (pending, no agent launched)
        }
        let (status, body) = get_response(
            &format!("/api/workflows/runs/{run_id}/steps/gate-step/log"),
            state,
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_error_contains(&body, "has no agent run");
    }

    #[tokio::test]
    async fn step_log_missing_file_returns_io_error() {
        let state = empty_state();
        let run_id = seed_workflow_fixtures(&state).await;
        {
            let db = state.db.lock().await;
            // Insert a second agent run to act as the child
            let ar2 = insert_agent_run(&db, "w1", "child", "running", "/nonexistent/path/log.txt");
            let mgr = WorkflowManager::new(&db);
            let step_id = mgr
                .insert_step(&run_id, "my-step", "actor", false, 0, 0)
                .unwrap();
            mgr.update_step_status(
                &step_id,
                conductor_core::workflow::WorkflowStepStatus::Running,
                Some(&ar2),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        }
        let (status, body) = get_response(
            &format!("/api/workflows/runs/{run_id}/steps/my-step/log"),
            state,
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_error_contains(&body, "/nonexistent/path/log.txt");
        assert_error_contains(&body, &run_id);
        assert_error_contains(&body, "my-step");
    }

    #[tokio::test]
    async fn step_log_happy_path_returns_log_content() {
        let state = empty_state();
        let run_id = seed_workflow_fixtures(&state).await;

        // Write a temp log file
        let log_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(log_file.path(), "hello from the agent").unwrap();
        let log_path = log_file.path().to_str().unwrap().to_string();

        {
            let db = state.db.lock().await;
            let ar2 = insert_agent_run(&db, "w1", "child", "running", &log_path);
            let mgr = WorkflowManager::new(&db);
            let step_id = mgr
                .insert_step(&run_id, "my-step", "actor", false, 0, 0)
                .unwrap();
            mgr.update_step_status(
                &step_id,
                conductor_core::workflow::WorkflowStepStatus::Running,
                Some(&ar2),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        }

        let (status, body) = get_response(
            &format!("/api/workflows/runs/{run_id}/steps/my-step/log"),
            state,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["log"], "hello from the agent");
    }

    #[tokio::test]
    async fn step_log_multi_iteration_returns_last_iteration() {
        let state = empty_state();
        let run_id = seed_workflow_fixtures(&state).await;

        // Write two log files — one for each iteration
        let log_iter0 = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(log_iter0.path(), "iteration 0 log").unwrap();
        let log_iter1 = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(log_iter1.path(), "iteration 1 log").unwrap();
        let path0 = log_iter0.path().to_str().unwrap().to_string();
        let path1 = log_iter1.path().to_str().unwrap().to_string();

        {
            let db = state.db.lock().await;
            // Agent run for iteration 0
            let ar_iter0 = insert_agent_run(&db, "w1", "child-iter0", "completed", &path0);
            // Agent run for iteration 1
            let ar_iter1 = insert_agent_run(&db, "w1", "child-iter1", "running", &path1);
            let mgr = WorkflowManager::new(&db);
            // Insert iteration 0 step
            let step0_id = mgr
                .insert_step(&run_id, "my-step", "actor", false, 0, 0)
                .unwrap();
            mgr.update_step_status(
                &step0_id,
                conductor_core::workflow::WorkflowStepStatus::Completed,
                Some(&ar_iter0),
                None,
                None,
                None,
                None,
            )
            .unwrap();
            // Insert iteration 1 step (same step_name, higher iteration)
            let step1_id = mgr
                .insert_step(&run_id, "my-step", "actor", false, 0, 1)
                .unwrap();
            mgr.update_step_status(
                &step1_id,
                conductor_core::workflow::WorkflowStepStatus::Running,
                Some(&ar_iter1),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        }

        let (status, body) = get_response(
            &format!("/api/workflows/runs/{run_id}/steps/my-step/log"),
            state,
        )
        .await;
        // Should return the log for the highest iteration (iteration 1)
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["log"], "iteration 1 log");
    }

    #[tokio::test]
    async fn step_log_three_iterations_returns_last() {
        let state = empty_state();
        let run_id = seed_workflow_fixtures(&state).await;

        let log_iter0 = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(log_iter0.path(), "iteration 0 log").unwrap();
        let log_iter1 = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(log_iter1.path(), "iteration 1 log").unwrap();
        let log_iter2 = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(log_iter2.path(), "iteration 2 log").unwrap();
        let path0 = log_iter0.path().to_str().unwrap().to_string();
        let path1 = log_iter1.path().to_str().unwrap().to_string();
        let path2 = log_iter2.path().to_str().unwrap().to_string();

        {
            let db = state.db.lock().await;
            for (path, iter) in [
                (path0.as_str(), 0i64),
                (path1.as_str(), 1i64),
                (path2.as_str(), 2i64),
            ] {
                let run_id_iter = insert_agent_run(&db, "w1", "child", "completed", path);
                let mgr = WorkflowManager::new(&db);
                let step_id = mgr
                    .insert_step(&run_id, "tri-step", "actor", false, 0, iter)
                    .unwrap();
                mgr.update_step_status(
                    &step_id,
                    conductor_core::workflow::WorkflowStepStatus::Completed,
                    Some(&run_id_iter),
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
            }
        }

        let (status, body) = get_response(
            &format!("/api/workflows/runs/{run_id}/steps/tri-step/log"),
            state,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["log"], "iteration 2 log");
    }

    #[tokio::test]
    async fn step_log_multi_step_name_isolation() {
        let state = empty_state();
        let run_id = seed_workflow_fixtures(&state).await;

        let log_build = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(log_build.path(), "build step log").unwrap();
        let log_deploy0 = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(log_deploy0.path(), "deploy iteration 0 log").unwrap();
        let log_deploy1 = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(log_deploy1.path(), "deploy iteration 1 log").unwrap();
        let path_build = log_build.path().to_str().unwrap().to_string();
        let path_deploy0 = log_deploy0.path().to_str().unwrap().to_string();
        let path_deploy1 = log_deploy1.path().to_str().unwrap().to_string();

        {
            let db = state.db.lock().await;
            // build step (iteration 0)
            let ar_iso_build = insert_agent_run(&db, "w1", "build", "completed", &path_build);
            let mgr = WorkflowManager::new(&db);
            let build_step_id = mgr
                .insert_step(&run_id, "build", "actor", false, 0, 0)
                .unwrap();
            mgr.update_step_status(
                &build_step_id,
                conductor_core::workflow::WorkflowStepStatus::Completed,
                Some(&ar_iso_build),
                None,
                None,
                None,
                None,
            )
            .unwrap();

            // deploy step iteration 0
            let ar_iso_dep0 = insert_agent_run(&db, "w1", "deploy0", "completed", &path_deploy0);
            let dep0_id = mgr
                .insert_step(&run_id, "deploy", "actor", false, 0, 0)
                .unwrap();
            mgr.update_step_status(
                &dep0_id,
                conductor_core::workflow::WorkflowStepStatus::Completed,
                Some(&ar_iso_dep0),
                None,
                None,
                None,
                None,
            )
            .unwrap();

            // deploy step iteration 1
            let ar_iso_dep1 = insert_agent_run(&db, "w1", "deploy1", "running", &path_deploy1);
            let dep1_id = mgr
                .insert_step(&run_id, "deploy", "actor", false, 0, 1)
                .unwrap();
            mgr.update_step_status(
                &dep1_id,
                conductor_core::workflow::WorkflowStepStatus::Running,
                Some(&ar_iso_dep1),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        }

        let (status, body) = get_response(
            &format!("/api/workflows/runs/{run_id}/steps/deploy/log"),
            state,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["log"], "deploy iteration 1 log");
    }

    #[test]
    fn status_filter_defaults_to_all() {
        let filter: StatusFilter = Default::default();
        let summaries = vec![
            WorkflowDefSummary {
                name: "valid-wf".to_string(),
                title: None,
                description: String::new(),
                trigger: String::new(),
                inputs: vec![],
                node_count: 1,
                group: None,
                targets: vec![],
                valid: true,
                error: None,
            },
            WorkflowDefSummary {
                name: "invalid-wf".to_string(),
                title: None,
                description: String::new(),
                trigger: String::new(),
                inputs: vec![],
                node_count: 0,
                group: None,
                targets: vec![],
                valid: false,
                error: Some("parse error".to_string()),
            },
        ];
        let result: Vec<WorkflowDefSummary> = match filter {
            StatusFilter::All => summaries,
            StatusFilter::Valid => unreachable!(),
            StatusFilter::Invalid => unreachable!(),
        };
        assert_eq!(result.len(), 2, "All filter should return both entries");
    }

    #[test]
    fn status_filter_valid_excludes_invalid() {
        let summaries = vec![
            WorkflowDefSummary {
                name: "good".to_string(),
                title: None,
                description: String::new(),
                trigger: String::new(),
                inputs: vec![],
                node_count: 1,
                group: None,
                targets: vec![],
                valid: true,
                error: None,
            },
            WorkflowDefSummary {
                name: "bad".to_string(),
                title: None,
                description: String::new(),
                trigger: String::new(),
                inputs: vec![],
                node_count: 0,
                group: None,
                targets: vec![],
                valid: false,
                error: Some("error".to_string()),
            },
        ];
        let valid: Vec<WorkflowDefSummary> = summaries.into_iter().filter(|s| s.valid).collect();
        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0].name, "good");
    }

    #[test]
    fn status_filter_invalid_excludes_valid() {
        let summaries = vec![
            WorkflowDefSummary {
                name: "good".to_string(),
                title: None,
                description: String::new(),
                trigger: String::new(),
                inputs: vec![],
                node_count: 1,
                group: None,
                targets: vec![],
                valid: true,
                error: None,
            },
            WorkflowDefSummary {
                name: "bad".to_string(),
                title: None,
                description: String::new(),
                trigger: String::new(),
                inputs: vec![],
                node_count: 0,
                group: None,
                targets: vec![],
                valid: false,
                error: Some("error".to_string()),
            },
        ];
        let invalid: Vec<WorkflowDefSummary> = summaries.into_iter().filter(|s| !s.valid).collect();
        assert_eq!(invalid.len(), 1);
        assert_eq!(invalid[0].name, "bad");
    }

    #[test]
    fn workflow_def_summary_includes_targets() {
        use conductor_core::workflow::{WorkflowDef, WorkflowTrigger};

        let def = WorkflowDef {
            name: "test-wf".to_string(),
            title: None,
            description: "A test workflow".to_string(),
            trigger: WorkflowTrigger::Manual,
            targets: vec!["repo".to_string(), "worktree".to_string()],
            group: None,
            inputs: vec![],
            body: vec![],
            always: vec![],
            source_path: "test.wf".to_string(),
        };

        let summary = WorkflowDefSummary::from(&def);
        assert_eq!(summary.targets, vec!["repo", "worktree"]);

        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["targets"], serde_json::json!(["repo", "worktree"]));
    }

    #[test]
    fn workflow_def_summary_empty_targets() {
        use conductor_core::workflow::{WorkflowDef, WorkflowTrigger};

        let def = WorkflowDef {
            name: "all-contexts-wf".to_string(),
            title: None,
            description: "Applies to all contexts".to_string(),
            trigger: WorkflowTrigger::Manual,
            targets: vec![],
            group: None,
            inputs: vec![],
            body: vec![],
            always: vec![],
            source_path: "all-contexts.wf".to_string(),
        };

        let summary = WorkflowDefSummary::from(&def);
        assert!(summary.targets.is_empty());

        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["targets"], serde_json::json!([]));
    }

    // ── build_workflow_summaries / callers ─────────────────────────────

    /// GET /api/repos/{id}/workflows returns 200 with empty array when the
    /// repo exists but has no workflow definitions on disk.
    #[tokio::test]
    async fn list_repo_workflow_defs_returns_empty_for_valid_repo() {
        let (status, body) = get_response("/api/repos/r1/workflows", seeded_state()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!([]));
    }

    /// GET /api/repos/{id}/workflows returns 404 when the repo does not exist.
    #[tokio::test]
    async fn list_repo_workflow_defs_returns_404_for_unknown_repo() {
        let (status, _) = get_response("/api/repos/nonexistent/workflows", seeded_state()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// GET /api/worktrees/{id}/workflows/defs returns 200 with empty array when
    /// the worktree exists but has no workflow definitions on disk.
    #[tokio::test]
    async fn list_workflow_defs_returns_empty_for_valid_worktree() {
        let (status, body) = get_response("/api/worktrees/w1/workflows/defs", seeded_state()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!([]));
    }

    /// GET /api/worktrees/{id}/workflows/defs returns 404 when the worktree
    /// does not exist.
    #[tokio::test]
    async fn list_workflow_defs_returns_404_for_unknown_worktree() {
        let (status, _) =
            get_response("/api/worktrees/nonexistent/workflows/defs", seeded_state()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// GET /api/worktrees/{id}/workflows/defs/{name} returns an error when the
    /// named workflow does not exist. Exercises the new `map_err(ApiError::Core)?`
    /// path in `get_workflow_def` — previously `unwrap_or_default()` silently
    /// returned an empty list; now `list_defs` errors propagate and a missing
    /// name produces a `ConductorError::Workflow` (mapped to 500 by `error.rs`).
    #[tokio::test]
    async fn get_workflow_def_returns_error_for_unknown_name() {
        let (status, _) = get_response(
            "/api/worktrees/w1/workflows/defs/no-such-workflow",
            seeded_state(),
        )
        .await;
        // ConductorError::Workflow is not in the 404 allowlist in error.rs, so
        // "not found" produces a 500. This is pre-existing behaviour for the
        // ok_or_else path; what we verify here is that the new `?` propagation
        // does not swallow the error silently.
        assert_ne!(status, StatusCode::OK);
    }

    /// GET /api/worktrees/{id}/workflows/defs/{name} returns 404 when the
    /// worktree itself does not exist.
    #[tokio::test]
    async fn get_workflow_def_returns_404_for_unknown_worktree() {
        let (status, _) = get_response(
            "/api/worktrees/nonexistent/workflows/defs/my-workflow",
            seeded_state(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
