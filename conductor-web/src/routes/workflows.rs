use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::error::ConductorError;
use conductor_core::feature::FeatureManager;
use conductor_core::repo::RepoManager;
use conductor_core::workflow::{
    apply_workflow_input_defaults, execute_workflow, validate_resume_preconditions, InputDecl,
    RunIdSlot, WorkflowDef, WorkflowExecConfig, WorkflowExecInput, WorkflowManager,
    WorkflowResumeStandalone, WorkflowRun, WorkflowRunStatus, WorkflowRunStep,
};
use conductor_core::worktree::WorktreeManager;

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::notify::fire_workflow_notification;
use crate::state::AppState;

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
    run_id: &str,
    workflow_name: &str,
    label: Option<&str>,
    succeeded: bool,
) {
    fire_workflow_notification(conn, notifications, run_id, workflow_name, label, succeeded);
}

// ── Response types ────────────────────────────────────────────────────

/// Web-layer wrapper that attaches active steps to a `WorkflowRun` for the list endpoint.
/// Preserves the exact JSON shape the frontend expects (active_steps is omitted when empty).
#[derive(Serialize)]
pub struct WorkflowRunResponse {
    #[serde(flatten)]
    run: WorkflowRun,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    active_steps: Vec<WorkflowRunStep>,
}

#[derive(Serialize)]
pub struct InputDeclSummary {
    pub name: String,
    pub required: bool,
    pub input_type: String,
    pub default: Option<String>,
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
        }
    }
}

#[derive(Serialize)]
pub struct WorkflowDefSummary {
    pub name: String,
    pub description: String,
    pub trigger: String,
    pub inputs: Vec<InputDeclSummary>,
    pub node_count: usize,
}

impl From<&WorkflowDef> for WorkflowDefSummary {
    fn from(def: &WorkflowDef) -> Self {
        Self {
            name: def.name.clone(),
            description: def.description.clone(),
            trigger: def.trigger.to_string(),
            inputs: def.inputs.iter().map(InputDeclSummary::from).collect(),
            node_count: def.body.len(),
        }
    }
}

// ── Request types ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RunWorkflowRequest {
    pub name: String,
    pub model: Option<String>,
    pub dry_run: Option<bool>,
    pub inputs: Option<HashMap<String, String>>,
    pub feature: Option<String>,
}

#[derive(Deserialize)]
pub struct ResumeWorkflowRequest {
    pub from_step: Option<String>,
    pub model: Option<String>,
    pub restart: Option<bool>,
}

#[derive(Deserialize)]
pub struct GateActionRequest {
    pub feedback: Option<String>,
}

// ── Endpoints ─────────────────────────────────────────────────────────

/// GET /api/worktrees/{id}/workflows/defs
pub async fn list_workflow_defs(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<Vec<WorkflowDefSummary>>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let wt_mgr = WorktreeManager::new(&db, &config);
    let wt = wt_mgr.get_by_id(&worktree_id)?;
    let repo = RepoManager::new(&db, &config).get_by_id(&wt.repo_id)?;

    let (defs, warnings) =
        WorkflowManager::list_defs(&wt.path, &repo.local_path).unwrap_or_default();
    for w in &warnings {
        tracing::warn!("Failed to parse {}: {}", w.file, w.message);
    }
    let summaries: Vec<WorkflowDefSummary> = defs.iter().map(WorkflowDefSummary::from).collect();
    Ok(Json(summaries))
}

/// POST /api/worktrees/{id}/workflows/run
pub async fn run_workflow(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
    Json(req): Json<RunWorkflowRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // Validate inputs while holding the lock
    let (wt_path, wt_slug, wt_ticket_id, repo_path, repo_slug, model, feature_id) = {
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
            return Err(ApiError(ConductorError::WorkflowRunAlreadyActive {
                name: active.workflow_name,
            }));
        }

        // Resolve model: request → per-worktree → per-repo config → global config
        let repo_config =
            conductor_core::config::RepoConfig::load(std::path::Path::new(&repo.local_path))
                .unwrap_or_default();
        let model = req
            .model
            .clone()
            .or_else(|| wt.model.clone())
            .or(repo_config.defaults.model)
            .or_else(|| config.general.model.clone());

        // Resolve feature_id synchronously so user-facing errors (e.g. ambiguous
        // features) are returned as HTTP errors before the 202 Accepted.
        let feature_id = FeatureManager::new(&db, &config).resolve_feature_id_for_run(
            req.feature.as_deref(),
            Some(&repo.slug),
            wt.ticket_id.as_deref(),
            Some(&wt.slug),
        )?;

        (
            wt.path.clone(),
            wt.slug.clone(),
            wt.ticket_id.clone(),
            repo.local_path.clone(),
            repo.slug.clone(),
            model,
            feature_id,
        )
    };

    let workflow_name = req.name.clone();
    let dry_run = req.dry_run.unwrap_or(false);
    let mut inputs = req.inputs.unwrap_or_default();
    let wt_id = worktree_id.clone();

    // Spawn background task to run the workflow
    let wt_target_label = format!("{repo_slug}/{wt_slug}");
    let state_clone = state.clone();
    tokio::task::spawn(async move {
        // Slot receives the real workflow run ULID once execute_workflow creates the
        // DB record. On the error path we prefer the real ULID (so dedup aligns with
        // any concurrent TUI notification keyed on the same ID); we fall back to the
        // deterministic bucket key only when no run record was created at all.
        let run_id_slot: RunIdSlot =
            std::sync::Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));

        let result = {
            let db = state_clone.db.lock().await;
            let config = state_clone.config.read().await;

            let def = match WorkflowManager::load_def_by_name(&wt_path, &repo_path, &workflow_name)
            {
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

            let exec_config = WorkflowExecConfig {
                dry_run,
                ..Default::default()
            };

            let input = WorkflowExecInput {
                conn: &db,
                config: &config,
                workflow: &def,
                worktree_id: Some(&wt_id),
                working_dir: &wt_path,
                repo_path: &repo_path,
                ticket_id: wt_ticket_id.as_deref(),
                repo_id: None,
                model: model.as_deref(),
                exec_config: &exec_config,
                inputs: inputs.clone(),
                depth: 0,
                parent_workflow_run_id: None,
                target_label: Some(&wt_target_label),
                default_bot_name: None,
                feature_id: feature_id.as_deref(),
                iteration: 0,
                run_id_notify: Some(std::sync::Arc::clone(&run_id_slot)),
            };

            execute_workflow(&input)
        };

        // Fire desktop notification off the async executor.
        // Use the cached config from AppState to avoid a redundant disk read.
        let notifications = state_clone.config.read().await.notifications.clone();

        match result {
            Ok(res) => {
                let succeeded = res.all_succeeded;
                let status = if succeeded { "completed" } else { "failed" };

                let wf_name = workflow_name.clone();
                let label = wt_target_label.clone();
                let notify_run_id = res.workflow_run_id.clone();
                tokio::task::spawn_blocking(move || {
                    let conn =
                        match conductor_core::db::open_database(&conductor_core::config::db_path())
                        {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::error!("notify: DB open failed: {e}");
                                return;
                            }
                        };
                    notify_workflow(
                        &conn,
                        &notifications,
                        &notify_run_id,
                        &wf_name,
                        Some(&label),
                        succeeded,
                    );
                });

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
                let wf_name = workflow_name.clone();
                let label = wt_target_label.clone();
                tokio::task::spawn_blocking(move || {
                    let error_run_id = resolve_error_run_id(&run_id_slot, &wf_name, &label);
                    let conn =
                        match conductor_core::db::open_database(&conductor_core::config::db_path())
                        {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::error!("notify: DB open failed: {e}");
                                return;
                            }
                        };
                    notify_workflow(
                        &conn,
                        &notifications,
                        &error_run_id,
                        &wf_name,
                        Some(&label),
                        false,
                    );
                });
            }
        }

        if let Some(notify) = &state_clone.workflow_done_notify {
            notify.notify_one();
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "started",
            "worktree_id": worktree_id,
        })),
    ))
}

/// Query params for GET /api/workflows/runs
#[derive(Deserialize)]
pub struct ListAllRunsQuery {
    /// Comma-separated list of statuses. Defaults to running, waiting, pending (owned by the manager layer).
    pub status: Option<String>,
}

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
                    return Err(ApiError(ConductorError::InvalidInput(
                        "empty status token in list".to_string(),
                    )));
                }
                WorkflowRunStatus::from_str(trimmed)
                    .map_err(|e| ApiError(ConductorError::InvalidInput(e)))
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    if !raw.is_empty() && statuses.is_empty() {
        return Err(ApiError(ConductorError::InvalidInput(
            "status filter yielded no valid values — did you pass only commas?".into(),
        )));
    }

    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let runs = mgr.list_active_workflow_runs(&statuses)?;

    // Batch-fetch only running/waiting steps for all runs (filter pushed to SQL)
    let run_ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    let mut steps_by_run = mgr.get_active_steps_for_runs(&run_ids)?;
    let responses: Vec<WorkflowRunResponse> = runs
        .into_iter()
        .map(|run| {
            let active_steps = steps_by_run.remove(&run.id).unwrap_or_default();
            WorkflowRunResponse { run, active_steps }
        })
        .collect();

    Ok(Json(responses))
}

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

/// GET /api/workflows/runs/{id}
pub async fn get_workflow_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<WorkflowRun>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let run = mgr.get_workflow_run(&id)?.ok_or_else(|| {
        ApiError(ConductorError::Workflow(format!(
            "Workflow run not found: {id}"
        )))
    })?;
    Ok(Json(run))
}

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

/// POST /api/workflows/runs/{id}/cancel
pub async fn cancel_workflow(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);

    // Verify run exists
    let run = mgr.get_workflow_run(&id)?.ok_or_else(|| {
        ApiError(ConductorError::Workflow(format!(
            "Workflow run not found: {id}"
        )))
    })?;

    mgr.update_workflow_status(&id, WorkflowRunStatus::Cancelled, Some("Cancelled by user"))?;

    state.events.emit(ConductorEvent::WorkflowRunStatusChanged {
        run_id: id.clone(),
        worktree_id: run.worktree_id.clone(),
        status: "cancelled".to_string(),
    });

    Ok(Json(
        serde_json::json!({ "status": "cancelled", "run_id": id }),
    ))
}

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
    let (workflow_name, target_label) = {
        let db = state.db.lock().await;
        let mgr = WorkflowManager::new(&db);
        let run = mgr.get_workflow_run(&id)?.ok_or_else(|| {
            ApiError(ConductorError::Workflow(format!(
                "Workflow run not found: {id}"
            )))
        })?;
        validate_resume_preconditions(&run.status, restart, from_step.as_deref())
            .map_err(ApiError)?;
        (run.workflow_name.clone(), run.target_label.clone())
    }; // DB lock released here

    // Spawn blocking task with its own DB connection (same pattern as run_workflow)
    let state_clone = state.clone();
    let run_id = id.clone();
    let notifications = config.notifications.clone();
    tokio::task::spawn_blocking(move || {
        let params = WorkflowResumeStandalone {
            config,
            workflow_run_id: run_id,
            model,
            from_step,
            restart,
            db_path: None,
        };

        let result = conductor_core::workflow::resume_workflow_standalone(&params);

        let conn = match conductor_core::db::open_database(&conductor_core::config::db_path()) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("notify: DB open failed: {e}");
                return;
            }
        };

        match result {
            Ok(res) => {
                let succeeded = res.all_succeeded;
                let status = if succeeded { "completed" } else { "failed" };

                notify_workflow(
                    &conn,
                    &notifications,
                    &res.workflow_run_id,
                    &workflow_name,
                    target_label.as_deref(),
                    succeeded,
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
                    &params.workflow_run_id,
                    &workflow_name,
                    target_label.as_deref(),
                    false,
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

/// POST /api/workflows/runs/{id}/gate/approve
pub async fn approve_gate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<GateActionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);

    let step = mgr.find_waiting_gate(&id)?.ok_or_else(|| {
        ApiError(ConductorError::Workflow(
            "No waiting gate found for this workflow run".to_string(),
        ))
    })?;

    mgr.approve_gate(&step.id, "user", req.feedback.as_deref())?;

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

/// POST /api/workflows/runs/{id}/gate/reject
pub async fn reject_gate(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);

    let step = mgr.find_waiting_gate(&id)?.ok_or_else(|| {
        ApiError(ConductorError::Workflow(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use conductor_core::config::Config;
    use conductor_core::workflow::WorkflowStepStatus;
    use tokio::sync::{Mutex, RwLock};
    use tower::ServiceExt;

    use crate::events::EventBus;
    use crate::routes::api_router;

    fn empty_state() -> AppState {
        let conn = conductor_core::test_helpers::create_test_conn();
        AppState {
            db: Arc::new(Mutex::new(conn)),
            config: Arc::new(RwLock::new(Config::default())),
            events: EventBus::new(1),
            workflow_done_notify: None,
        }
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
    async fn active_steps_attached_filters_to_running_and_waiting() {
        let state = empty_state();
        {
            let db = state.db.lock().await;

            // Seed the minimum fixtures required by the FK chain:
            // workflow_runs.parent_run_id → agent_runs.id → worktrees.id → repos.id
            db.execute_batch(
                "INSERT INTO repos (id, slug, local_path, remote_url, workspace_dir, created_at) \
                 VALUES ('r1', 'test-repo', '/tmp/repo', 'https://github.com/test/repo.git', '/tmp/ws', '2024-01-01T00:00:00Z');
                 INSERT INTO worktrees (id, repo_id, slug, branch, path, status, created_at) \
                 VALUES ('w1', 'r1', 'feat-test', 'feat/test', '/tmp/ws/feat-test', 'active', '2024-01-01T00:00:00Z');
                 INSERT INTO agent_runs (id, worktree_id, prompt, status, started_at) \
                 VALUES ('ar1', 'w1', 'test', 'running', '2024-01-01T00:00:00Z');",
            )
            .unwrap();

            let mgr = WorkflowManager::new(&db);
            // worktree_id = None so the run is visible without an active worktree join
            let run = mgr
                .create_workflow_run("test-wf", None, "ar1", false, "manual", None)
                .unwrap();
            mgr.update_workflow_status(&run.id, WorkflowRunStatus::Running, None)
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
            notify_workflow(&conn, &notifications, "test-run-id", "test-wf", None, false);
        })
        .await
        .unwrap();
    }

    fn test_notification_config() -> conductor_core::config::NotificationConfig {
        conductor_core::config::NotificationConfig {
            enabled: true,
            workflows: conductor_core::config::WorkflowNotificationConfig {
                on_success: true,
                on_failure: true,
                on_gate_human: true,
                on_gate_ci: false,
                on_gate_pr_review: true,
            },
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
                &key1,
                "my-workflow",
                Some("repo/wt"),
                false,
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
                &key2,
                "my-workflow",
                Some("repo/wt"),
                false,
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
            notify_workflow(&conn, &notifications, "run-notify-1", "my-workflow", None, true);

            // Verify the dedup row was inserted into notification_log
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM notification_log WHERE entity_id = 'run-notify-1' AND event_type = 'completed'",
                    [],
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
    /// the end-to-end wiring: the handler must call execute_workflow (which populates
    /// the run_id_notify slot and creates a workflow_runs record).
    ///
    /// If `run_id_notify: Some(...)` is ever dropped from the WorkflowExecInput
    /// construction in the handler, execute_workflow will still create the run
    /// record — so this test acts as a broader regression guard that the handler
    /// successfully invokes execute_workflow and the DB state is consistent.
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

        let notify = Arc::new(tokio::sync::Notify::new());
        let state = AppState {
            workflow_done_notify: Some(Arc::clone(&notify)),
            ..empty_state()
        };
        {
            let db = state.db.lock().await;
            db.execute_batch(&format!(
                "INSERT INTO repos \
                     (id, slug, local_path, remote_url, default_branch, workspace_dir, created_at) \
                     VALUES ('r1', 'test-repo', '{wt_path}', \
                             'https://github.com/test/repo.git', 'main', '/tmp/ws', \
                             '2024-01-01T00:00:00Z'); \
                 INSERT INTO worktrees \
                     (id, repo_id, slug, branch, path, status, created_at) \
                     VALUES ('w1', 'r1', 'feat-test', 'feat/test', '{wt_path}', 'active', \
                             '2024-01-01T00:00:00Z');",
            ))
            .unwrap();
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

        // Wait deterministically for the background task to signal completion.
        tokio::time::timeout(std::time::Duration::from_secs(5), notify.notified())
            .await
            .expect("run_workflow background task did not complete within 5 s");

        let db = state.db.lock().await;
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM workflow_runs WHERE workflow_name = 'noop'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "handler must invoke execute_workflow — no workflow_runs row found for 'noop'"
        );
    }
}
