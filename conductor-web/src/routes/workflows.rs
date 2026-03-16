use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::error::ConductorError;
use conductor_core::repo::RepoManager;
use conductor_core::workflow::{
    apply_workflow_input_defaults, execute_workflow, validate_resume_preconditions, InputDecl,
    WorkflowDef, WorkflowExecConfig, WorkflowExecInput, WorkflowManager, WorkflowResumeStandalone,
    WorkflowRun, WorkflowRunStatus, WorkflowRunStep,
};
use conductor_core::worktree::WorktreeManager;

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

// ── Response types ────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct InputDeclSummary {
    pub name: String,
    pub required: bool,
}

impl From<&InputDecl> for InputDeclSummary {
    fn from(d: &InputDecl) -> Self {
        Self {
            name: d.name.clone(),
            required: d.required,
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
    let (wt_path, wt_slug, repo_path, repo_slug, model) = {
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
            repo.local_path.clone(),
            repo.slug.clone(),
            model,
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
                ticket_id: None,
                repo_id: None,
                model: model.as_deref(),
                exec_config: &exec_config,
                inputs: inputs.clone(),
                depth: 0,
                parent_workflow_run_id: None,
                target_label: Some(&wt_target_label),
                default_bot_name: None,
                run_id_notify: None,
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
                tokio::task::spawn_blocking(move || {
                    crate::notify::fire_workflow_notification(
                        &notifications,
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
                    crate::notify::fire_workflow_notification(
                        &notifications,
                        &wf_name,
                        Some(&label),
                        false,
                    );
                });
            }
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
) -> Result<Json<Vec<WorkflowRun>>, ApiError> {
    use std::str::FromStr;

    let statuses: Vec<WorkflowRunStatus> = params
        .status
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| {
            WorkflowRunStatus::from_str(s.trim())
                .map_err(|e| ApiError(ConductorError::InvalidInput(e)))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let db = state.db.lock().await;
    let mgr = WorkflowManager::new(&db);
    let runs = mgr.list_active_workflow_runs(&statuses)?;
    Ok(Json(runs))
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

        match result {
            Ok(res) => {
                let succeeded = res.all_succeeded;
                let status = if succeeded { "completed" } else { "failed" };

                crate::notify::fire_workflow_notification(
                    &notifications,
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
                crate::notify::fire_workflow_notification(
                    &notifications,
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
    use tokio::sync::{Mutex, RwLock};
    use tower::ServiceExt;

    use crate::events::EventBus;
    use crate::routes::api_router;

    fn empty_state() -> AppState {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conductor_core::db::migrations::run(&conn).unwrap();
        AppState {
            db: Arc::new(Mutex::new(conn)),
            config: Arc::new(RwLock::new(Config::default())),
            events: EventBus::new(1),
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

    #[tokio::test]
    async fn status_valid_returns_200() {
        let (status, _) = get_response("/api/workflows/runs?status=running", empty_state()).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn status_bogus_returns_400() {
        let (status, body) = get_response("/api/workflows/runs?status=bogus", empty_state()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]
                .as_str()
                .unwrap_or("")
                .contains("unknown WorkflowRunStatus: bogus"),
            "unexpected error body: {body}"
        );
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
}
