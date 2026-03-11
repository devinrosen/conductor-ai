use std::collections::HashMap;

use axum::extract::{Path, State};
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

    let defs = WorkflowManager::list_defs(&wt.path, &repo.local_path).unwrap_or_default();
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
    let (wt_path, repo_path, model) = {
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

        (wt.path.clone(), repo.local_path.clone(), model)
    };

    let workflow_name = req.name.clone();
    let dry_run = req.dry_run.unwrap_or(false);
    let mut inputs = req.inputs.unwrap_or_default();
    let wt_id = worktree_id.clone();

    // Spawn background task to run the workflow
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
            };

            execute_workflow(&input)
        };

        match result {
            Ok(res) => {
                let status = if res.all_succeeded {
                    "completed"
                } else {
                    "failed"
                };
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

    // Validate the run exists and is in a resumable state before spawning
    {
        let db = state.db.lock().await;
        let mgr = WorkflowManager::new(&db);
        let run = mgr.get_workflow_run(&id)?.ok_or_else(|| {
            ApiError(ConductorError::Workflow(format!(
                "Workflow run not found: {id}"
            )))
        })?;
        validate_resume_preconditions(&run.status, restart, from_step.as_deref())
            .map_err(ApiError)?;
    } // DB lock released here

    // Spawn blocking task with its own DB connection (same pattern as run_workflow)
    let state_clone = state.clone();
    let run_id = id.clone();
    tokio::task::spawn_blocking(move || {
        let params = WorkflowResumeStandalone {
            config,
            workflow_run_id: run_id,
            model,
            from_step,
            restart,
        };

        let result = conductor_core::workflow::resume_workflow_standalone(&params);

        match result {
            Ok(res) => {
                let status = if res.all_succeeded {
                    "completed"
                } else {
                    "failed"
                };
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

    mgr.reject_gate(&step.id, "user")?;

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
