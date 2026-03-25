use std::collections::HashMap;
use std::process::Command;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::agent::{
    parse_agent_log, AgentCreatedIssue, AgentEvent, AgentManager, AgentRun, AgentRunEvent,
    FeedbackRequest, RunTreeTotals, TicketAgentTotals,
};
use conductor_core::error::ConductorError;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::{build_agent_prompt, TicketSyncer};
use conductor_core::worktree::WorktreeManager;

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

// ── Agent stats (aggregates) ──────────────────────────────────────────

pub async fn list_agent_runs(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<Vec<AgentRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let runs = mgr.list_for_worktree(&worktree_id)?;
    Ok(Json(runs))
}

pub async fn latest_runs_by_worktree(
    State(state): State<AppState>,
) -> Result<Json<HashMap<String, AgentRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let map = mgr.latest_runs_by_worktree()?;
    Ok(Json(map))
}

pub async fn ticket_totals(
    State(state): State<AppState>,
) -> Result<Json<HashMap<String, TicketAgentTotals>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let map = mgr.totals_by_ticket_all()?;
    Ok(Json(map))
}

pub async fn latest_runs_by_worktree_for_repo(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<HashMap<String, AgentRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let map = mgr.latest_runs_by_worktree_for_repo(&repo_id)?;
    Ok(Json(map))
}

pub async fn ticket_totals_for_repo(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<HashMap<String, TicketAgentTotals>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let map = mgr.totals_by_ticket_for_repo(&repo_id)?;
    Ok(Json(map))
}

// ── Agent orchestration ───────────────────────────────────────────────

/// List all agent runs for a worktree (newest first).
pub async fn list_runs(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<Vec<AgentRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let runs = mgr.list_for_worktree(&worktree_id)?;
    Ok(Json(runs))
}

/// Get the latest agent run for a worktree.
pub async fn latest_run(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<Option<AgentRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let run = mgr.latest_for_worktree(&worktree_id)?;
    Ok(Json(run))
}

#[derive(Deserialize)]
pub struct StartAgentRequest {
    pub prompt: String,
    pub resume_session_id: Option<String>,
    pub parent_run_id: Option<String>,
}

/// Start an agent for a worktree. Creates a DB record and spawns a tmux window.
pub async fn start_agent(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
    Json(body): Json<StartAgentRequest>,
) -> Result<(StatusCode, Json<AgentRun>), ApiError> {
    let db = state.db.lock().await;

    // Look up the worktree to get slug and path
    let config = state.config.read().await;
    let wt_mgr = WorktreeManager::new(&db, &config);
    let wt = wt_mgr.get_by_id(&worktree_id)?;

    // Check if there's already a running agent
    let agent_mgr = AgentManager::new(&db);
    if let Some(existing) = agent_mgr.latest_for_worktree(&worktree_id)? {
        if existing.is_active() {
            return Err(conductor_core::error::ConductorError::Agent(
                "Agent already running for this worktree".to_string(),
            )
            .into());
        }
    }

    // Resolve model: per-worktree → per-repo config → global config
    let repo = RepoManager::new(&db, &config).get_by_id(&wt.repo_id)?;
    let model = wt
        .model
        .as_deref()
        .or(repo.model.as_deref())
        .or(config.general.model.as_deref())
        .map(str::to_string);

    // Create DB record (child or top-level)
    let run = if let Some(ref parent_id) = body.parent_run_id {
        agent_mgr.create_child_run(
            Some(&worktree_id),
            &body.prompt,
            Some(&wt.slug),
            model.as_deref(),
            parent_id,
            None,
        )?
    } else {
        agent_mgr.create_run(
            Some(&worktree_id),
            &body.prompt,
            Some(&wt.slug),
            model.as_deref(),
        )?
    };

    // Build conductor agent run command
    let args = conductor_core::agent_runtime::build_agent_args(
        &run.id,
        &wt.path,
        &body.prompt,
        body.resume_session_id.as_deref(),
        model.as_deref(),
        None,
    )
    .map_err(|e| {
        let _ = agent_mgr.update_run_failed(&run.id, &e);
        ConductorError::Agent(e)
    })?;

    match conductor_core::agent_runtime::spawn_tmux_window(&args, &wt.slug) {
        Ok(()) => {
            state.events.emit(ConductorEvent::AgentStarted {
                run_id: run.id.clone(),
                worktree_id: wt.id.clone(),
            });
            Ok((StatusCode::CREATED, Json(run)))
        }
        Err(e) => {
            let _ = agent_mgr.update_run_failed(&run.id, &e);
            Err(conductor_core::error::ConductorError::Agent(e).into())
        }
    }
}

/// Stop a running agent: capture scrollback, kill tmux, mark cancelled.
pub async fn stop_agent(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<AgentRun>, ApiError> {
    let db = state.db.lock().await;
    let agent_mgr = AgentManager::new(&db);

    let run = agent_mgr
        .latest_for_worktree(&worktree_id)?
        .ok_or_else(|| {
            conductor_core::error::ConductorError::Agent(
                "No agent run found for this worktree".to_string(),
            )
        })?;

    if !run.is_active() {
        return Err(conductor_core::error::ConductorError::Agent(
            "Agent is not running".to_string(),
        )
        .into());
    }

    cancel_agent_run(&agent_mgr, &run)?;

    // Re-fetch to get updated record
    let updated = agent_mgr.latest_for_worktree(&worktree_id)?.unwrap_or(run);

    state.events.emit(ConductorEvent::AgentStopped {
        run_id: updated.id.clone(),
        worktree_id: worktree_id.clone(),
    });

    Ok(Json(updated))
}

#[derive(Serialize)]
pub struct AgentEventResponse {
    pub id: String,
    pub run_id: String,
    pub kind: String,
    pub summary: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub duration_ms: Option<i64>,
    pub metadata: Option<String>,
}

impl From<AgentRunEvent> for AgentEventResponse {
    fn from(e: AgentRunEvent) -> Self {
        let duration_ms = e.duration_ms();
        Self {
            id: e.id,
            run_id: e.run_id,
            kind: e.kind,
            summary: e.summary,
            started_at: e.started_at,
            ended_at: e.ended_at,
            duration_ms,
            metadata: e.metadata,
        }
    }
}

impl From<AgentEvent> for AgentEventResponse {
    fn from(e: AgentEvent) -> Self {
        Self {
            id: String::new(),
            run_id: String::new(),
            kind: e.kind,
            summary: e.summary,
            started_at: String::new(),
            ended_at: None,
            duration_ms: None,
            metadata: None,
        }
    }
}

/// Get parsed agent events for all runs of a worktree.
/// Uses DB records when available; falls back to log file parsing for older runs.
pub async fn get_events(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<Vec<AgentEventResponse>>, ApiError> {
    let db = state.db.lock().await;
    let wt_path = {
        let config = state.config.read().await;
        WorktreeManager::new(&db, &config)
            .get_by_id(&worktree_id)
            .map(|wt| wt.path)
            .unwrap_or_default()
    };
    let agent_mgr = AgentManager::new(&db);

    // Try DB events first (covers all runs with persisted events)
    let db_events = agent_mgr.list_events_for_worktree(&worktree_id)?;
    if !db_events.is_empty() {
        return Ok(Json(
            db_events
                .into_iter()
                .map(|e| {
                    let mut resp = AgentEventResponse::from(e);
                    resp.summary = strip_worktree_prefix(&resp.summary, &wt_path);
                    resp
                })
                .collect(),
        ));
    }

    // Backward compat: parse log files for older runs without DB events
    let runs = agent_mgr.list_for_worktree(&worktree_id)?;
    let mut all_events = Vec::new();
    for run in runs.iter().rev() {
        if let Some(ref log_file) = run.log_file {
            let events = parse_agent_log(log_file);
            all_events.extend(events.into_iter().map(|e| {
                let mut resp = AgentEventResponse::from(e);
                resp.summary = strip_worktree_prefix(&resp.summary, &wt_path);
                resp
            }));
        }
    }

    Ok(Json(all_events))
}

/// Get events for a specific agent run.
pub async fn get_run_events(
    State(state): State<AppState>,
    Path((worktree_id, run_id)): Path<(String, String)>,
) -> Result<Json<Vec<AgentEventResponse>>, ApiError> {
    let db = state.db.lock().await;
    let wt_path = {
        let config = state.config.read().await;
        WorktreeManager::new(&db, &config)
            .get_by_id(&worktree_id)
            .map(|wt| wt.path)
            .unwrap_or_default()
    };
    let agent_mgr = AgentManager::new(&db);

    let db_events = agent_mgr.list_events_for_run(&run_id)?;
    if !db_events.is_empty() {
        return Ok(Json(
            db_events
                .into_iter()
                .map(|e| {
                    let mut resp = AgentEventResponse::from(e);
                    resp.summary = strip_worktree_prefix(&resp.summary, &wt_path);
                    resp
                })
                .collect(),
        ));
    }

    // Backward compat: parse log file if no DB events
    let run = agent_mgr.get_run(&run_id)?;
    let events = run
        .and_then(|r| r.log_file)
        .map(|path| {
            parse_agent_log(&path)
                .into_iter()
                .map(|e| {
                    let mut resp = AgentEventResponse::from(e);
                    resp.summary = strip_worktree_prefix(&resp.summary, &wt_path);
                    resp
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(Json(events))
}

#[derive(Serialize)]
pub struct AgentPromptResponse {
    pub prompt: String,
    pub resume_session_id: Option<String>,
    /// True if the latest run ended with incomplete plan steps and can be auto-resumed.
    pub needs_resume: bool,
    /// Number of incomplete plan steps remaining (0 if no resume needed).
    pub incomplete_steps: usize,
}

/// Get a pre-filled agent prompt for a worktree (from its linked ticket).
pub async fn get_prompt(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<AgentPromptResponse>, ApiError> {
    let db = state.db.lock().await;

    // Look up worktree to get ticket_id
    let config = state.config.read().await;
    let wt_mgr = WorktreeManager::new(&db, &config);
    let wt = wt_mgr.get_by_id(&worktree_id)?;

    // Check for resumable session
    let agent_mgr = AgentManager::new(&db);
    let latest_run = agent_mgr.latest_for_worktree(&wt.id)?;

    let (resume_session_id, needs_resume, incomplete_steps) = match &latest_run {
        Some(run) if run.needs_resume() => {
            let incomplete = run.incomplete_plan_steps().len();
            (run.claude_session_id.clone(), true, incomplete)
        }
        Some(run) => (run.claude_session_id.clone(), false, 0),
        None => (None, false, 0),
    };

    // Build prompt: if needs_resume, use the resume prompt; otherwise use ticket context
    let prompt = if needs_resume {
        latest_run
            .as_ref()
            .map(|r| r.build_resume_prompt())
            .unwrap_or_default()
    } else if let Some(ref ticket_id) = wt.ticket_id {
        let syncer = TicketSyncer::new(&db);
        match syncer.get_by_id(ticket_id) {
            Ok(ticket) => build_agent_prompt(&ticket),
            Err(_) => String::new(),
        }
    } else {
        String::new()
    };

    Ok(Json(AgentPromptResponse {
        prompt,
        resume_session_id,
        needs_resume,
        incomplete_steps,
    }))
}

// ── Parent/child run tree endpoints ───────────────────────────────────

/// List direct child runs of a given parent run.
pub async fn list_child_runs(
    State(state): State<AppState>,
    Path((_worktree_id, run_id)): Path<(String, String)>,
) -> Result<Json<Vec<AgentRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let children = mgr.list_child_runs(&run_id)?;
    Ok(Json(children))
}

/// Get a full run tree: the root run plus all descendants.
pub async fn get_run_tree(
    State(state): State<AppState>,
    Path((_worktree_id, run_id)): Path<(String, String)>,
) -> Result<Json<Vec<AgentRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let tree = mgr.get_run_tree(&run_id)?;
    Ok(Json(tree))
}

/// Get aggregated cost/turns/duration for a run tree.
pub async fn get_run_tree_totals(
    State(state): State<AppState>,
    Path((_worktree_id, run_id)): Path<(String, String)>,
) -> Result<Json<RunTreeTotals>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let totals = mgr.aggregate_run_tree(&run_id)?;
    Ok(Json(totals))
}

/// List issues created by all agent runs for a worktree.
pub async fn list_created_issues(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<Vec<AgentCreatedIssue>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let issues = mgr.list_created_issues_for_worktree(&worktree_id)?;
    Ok(Json(issues))
}

// ── Agent orchestration (auto-spawn child runs) ──────────────────────

#[derive(Deserialize)]
pub struct OrchestrateRequest {
    pub prompt: String,
    /// Stop on first child failure.
    #[serde(default)]
    pub fail_fast: bool,
    /// Child run timeout in seconds (default: 1800 = 30 min).
    #[serde(default = "default_child_timeout_secs")]
    pub child_timeout_secs: u64,
}

fn default_child_timeout_secs() -> u64 {
    1800
}

/// Start an orchestrated agent run: generate a plan, then spawn child agents
/// for each step sequentially. The orchestrator runs in a tmux window.
pub async fn orchestrate_agent(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
    Json(body): Json<OrchestrateRequest>,
) -> Result<(StatusCode, Json<AgentRun>), ApiError> {
    let db = state.db.lock().await;

    // Look up the worktree
    let config = state.config.read().await;
    let wt_mgr = WorktreeManager::new(&db, &config);
    let wt = wt_mgr.get_by_id(&worktree_id)?;

    // Check if there's already a running agent
    let agent_mgr = AgentManager::new(&db);
    if let Some(existing) = agent_mgr.latest_for_worktree(&worktree_id)? {
        if existing.is_active() {
            return Err(conductor_core::error::ConductorError::Agent(
                "Agent already running for this worktree".to_string(),
            )
            .into());
        }
    }

    // Resolve model: per-worktree → per-repo config → global config
    let repo = RepoManager::new(&db, &config).get_by_id(&wt.repo_id)?;
    let model = wt
        .model
        .as_deref()
        .or(repo.model.as_deref())
        .or(config.general.model.as_deref())
        .map(str::to_string);

    // Create parent run record (this is the orchestrator run)
    let run = agent_mgr.create_run(
        Some(&worktree_id),
        &body.prompt,
        Some(&wt.slug),
        model.as_deref(),
    )?;

    // Build conductor agent orchestrate command
    let args = conductor_core::agent_runtime::build_orchestrate_args(
        &run.id,
        &wt.path,
        model.as_deref(),
        body.fail_fast,
        Some(body.child_timeout_secs),
    );

    match conductor_core::agent_runtime::spawn_tmux_window(&args, &wt.slug) {
        Ok(()) => {
            state.events.emit(ConductorEvent::AgentStarted {
                run_id: run.id.clone(),
                worktree_id: wt.id.clone(),
            });
            Ok((StatusCode::CREATED, Json(run)))
        }
        Err(e) => {
            let _ = agent_mgr.update_run_failed(&run.id, &e);
            Err(conductor_core::error::ConductorError::Agent(e).into())
        }
    }
}

// ── Feedback (human-in-the-loop) ──────────────────────────────────────

/// Get the pending feedback request for a worktree's running agent (if any).
pub async fn get_pending_feedback(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<Option<FeedbackRequest>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let feedback = mgr.pending_feedback_for_worktree(&worktree_id)?;
    Ok(Json(feedback))
}

/// List all feedback requests for a specific run.
pub async fn list_run_feedback(
    State(state): State<AppState>,
    Path((_worktree_id, run_id)): Path<(String, String)>,
) -> Result<Json<Vec<FeedbackRequest>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let feedback = mgr.list_feedback_for_run(&run_id)?;
    Ok(Json(feedback))
}

#[derive(Deserialize)]
pub struct RequestFeedbackBody {
    pub prompt: String,
}

/// Create a feedback request for a running agent (pauses the agent).
pub async fn request_feedback(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
    Json(body): Json<RequestFeedbackBody>,
) -> Result<(StatusCode, Json<FeedbackRequest>), ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);

    let run = mgr.latest_for_worktree(&worktree_id)?.ok_or_else(|| {
        conductor_core::error::ConductorError::Agent(
            "No agent run found for this worktree".to_string(),
        )
    })?;

    if run.status != conductor_core::agent::AgentRunStatus::Running {
        return Err(conductor_core::error::ConductorError::Agent(
            "Agent is not running".to_string(),
        )
        .into());
    }

    let feedback = mgr.request_feedback(&run.id, &body.prompt, None)?;

    state.events.emit(ConductorEvent::FeedbackRequested {
        run_id: run.id.clone(),
        worktree_id: worktree_id.clone(),
        feedback_id: feedback.id.clone(),
    });

    Ok((StatusCode::CREATED, Json(feedback)))
}

#[derive(Deserialize)]
pub struct SubmitFeedbackBody {
    pub response: String,
}

/// Submit a response to a pending feedback request (resumes the agent).
pub async fn submit_feedback(
    State(state): State<AppState>,
    Path((worktree_id, feedback_id)): Path<(String, String)>,
    Json(body): Json<SubmitFeedbackBody>,
) -> Result<Json<FeedbackRequest>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);

    // Verify the feedback belongs to this worktree
    verify_feedback_ownership(&mgr, &feedback_id, &worktree_id)?;

    let feedback = mgr.submit_feedback(&feedback_id, &body.response)?;

    state.events.emit(ConductorEvent::FeedbackSubmitted {
        run_id: feedback.run_id.clone(),
        worktree_id: worktree_id.clone(),
        feedback_id: feedback.id.clone(),
    });

    Ok(Json(feedback))
}

/// Dismiss a pending feedback request without responding (resumes the agent).
pub async fn dismiss_feedback(
    State(state): State<AppState>,
    Path((worktree_id, feedback_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);

    // Verify the feedback belongs to this worktree
    verify_feedback_ownership(&mgr, &feedback_id, &worktree_id)?;

    // Get the feedback to find the run_id for the SSE event
    let feedback = mgr.get_feedback(&feedback_id)?;

    mgr.dismiss_feedback(&feedback_id)?;

    if let Some(fb) = feedback {
        state.events.emit(ConductorEvent::FeedbackSubmitted {
            run_id: fb.run_id,
            worktree_id: worktree_id.clone(),
            feedback_id: feedback_id.clone(),
        });
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Verify that a feedback request belongs to the given worktree (via its run).
fn verify_feedback_ownership(
    mgr: &AgentManager,
    feedback_id: &str,
    worktree_id: &str,
) -> Result<(), ApiError> {
    let fb = mgr
        .get_feedback(feedback_id)?
        .ok_or_else(|| ApiError(ConductorError::Agent("feedback request not found".into())))?;
    let run = mgr
        .get_run(&fb.run_id)?
        .ok_or_else(|| ApiError(ConductorError::Agent("agent run not found".into())))?;
    if run.worktree_id.as_deref() != Some(worktree_id) {
        return Err(ApiError(ConductorError::Agent(
            "feedback request does not belong to this worktree".into(),
        )));
    }
    Ok(())
}

fn strip_worktree_prefix(summary: &str, worktree_path: &str) -> String {
    if worktree_path.is_empty() {
        return summary.to_string();
    }
    match summary.strip_prefix(worktree_path) {
        Some(rest) => format!("{{worktree}}{rest}"),
        None => summary.to_string(),
    }
}

/// Restart a failed/cancelled agent run by creating a new run with the
/// same prompt/config and re-spawning a tmux window.
pub async fn restart_agent(
    State(state): State<AppState>,
    Path((worktree_id, run_id)): Path<(String, String)>,
) -> Result<(StatusCode, Json<AgentRun>), ApiError> {
    // Scope DB + config access so locks are dropped before the blocking spawn.
    let (new_run, args, window_name) = {
        let db = state.db.lock().await;
        let config = state.config.read().await;

        let agent_mgr = AgentManager::new(&db);

        let new_run = agent_mgr.restart_run(&run_id)?;

        // Validate that run_id belongs to the URL worktree_id (IDOR guard).
        // Use the new_run's worktree_id (copied from the original) as source of truth.
        if new_run.worktree_id.as_deref() != Some(worktree_id.as_str()) {
            let _ = agent_mgr
                .update_run_failed(&new_run.id, "run does not belong to the specified worktree");
            return Err(ConductorError::Agent(
                "run does not belong to the specified worktree".to_string(),
            )
            .into());
        }

        // Resolve worktree path from new_run.worktree_id (single source of truth).
        let wt_mgr = WorktreeManager::new(&db, &config);
        let wt = wt_mgr.get_by_id(
            new_run
                .worktree_id
                .as_deref()
                .expect("worktree_id verified above"),
        )?;

        let window_name = new_run
            .tmux_window
            .as_deref()
            .unwrap_or(&wt.slug)
            .to_string();

        let args = conductor_core::agent_runtime::build_agent_args(
            &new_run.id,
            &wt.path,
            &new_run.prompt,
            None,
            new_run.model.as_deref(),
            new_run.bot_name.as_deref(),
        )
        .map_err(|e| {
            let _ = agent_mgr.update_run_failed(&new_run.id, &e);
            ConductorError::Agent(e)
        })?;

        (new_run, args, window_name)
    };
    // DB and config locks are now dropped.

    // Spawn tmux off the async runtime thread to avoid blocking the executor.
    let spawn_window = window_name.clone();
    let spawn_result = tokio::task::spawn_blocking(move || {
        conductor_core::agent_runtime::spawn_tmux_window(&args, &spawn_window)
    })
    .await
    .map_err(|e| ConductorError::Agent(format!("spawn task panicked: {e}")))?;

    match spawn_result {
        Ok(()) => {
            state.events.emit(ConductorEvent::AgentRestarted {
                run_id: new_run.id.clone(),
                old_run_id: run_id,
                worktree_id: worktree_id.clone(),
            });
            Ok((StatusCode::CREATED, Json(new_run)))
        }
        Err(e) => {
            let db = state.db.lock().await;
            let agent_mgr = AgentManager::new(&db);
            let _ = agent_mgr.update_run_failed(&new_run.id, &e);
            Err(ConductorError::Agent(e).into())
        }
    }
}

/// Capture tmux log, kill tmux window, and mark run as cancelled.
fn cancel_agent_run(mgr: &AgentManager, run: &AgentRun) -> Result<(), ApiError> {
    if let Some(ref window) = run.tmux_window {
        mgr.capture_agent_log(&run.id, window);
        let _ = Command::new("tmux")
            .args(["kill-window", "-t", &format!(":{window}")])
            .output();
    }
    mgr.update_run_cancelled(&run.id)?;
    Ok(())
}

// ── Repo-scoped agent routes ────────────────────────────────────────────

#[derive(Deserialize)]
pub struct StartRepoAgentRequest {
    pub prompt: String,
    /// When true, ignore any prior session and start fresh.
    #[serde(default)]
    pub new_session: bool,
}

/// Start a read-only agent scoped to a repo. Uses `--permission-mode plan`.
pub async fn start_repo_agent(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    Json(body): Json<StartRepoAgentRequest>,
) -> Result<(StatusCode, Json<AgentRun>), ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let repo = RepoManager::new(&db, &config).get_by_id(&repo_id)?;

    // Resolve model: per-repo config → global config
    let model = repo
        .model
        .as_deref()
        .or(config.general.model.as_deref())
        .map(str::to_string);

    let agent_mgr = AgentManager::new(&db);

    // Auto-resume: look up the latest repo-scoped session unless new_session is requested
    let resume_session_id = if body.new_session {
        None
    } else {
        agent_mgr
            .latest_repo_scoped(&repo_id)?
            .and_then(|run| run.claude_session_id)
    };

    // Tmux window name: repo-<slug>-<short_id>
    let run_id = conductor_core::new_id();
    let window_name = conductor_core::agent_runtime::repo_agent_window_name(&repo.slug, &run_id);

    let run =
        agent_mgr.create_repo_run(&repo_id, &body.prompt, Some(&window_name), model.as_deref())?;

    // Build args with plan permission mode (read-only)
    let plan_mode = conductor_core::config::AgentPermissionMode::Plan;
    let args = conductor_core::agent_runtime::build_agent_args_with_mode(
        &run.id,
        &repo.local_path,
        &body.prompt,
        resume_session_id.as_deref(),
        model.as_deref(),
        None,
        Some(&plan_mode),
    )
    .map_err(|e| {
        let _ = agent_mgr.update_run_failed(&run.id, &e);
        ConductorError::Agent(e)
    })?;

    match conductor_core::agent_runtime::spawn_tmux_window(&args, &window_name) {
        Ok(()) => {
            state.events.emit(ConductorEvent::RepoAgentStarted {
                run_id: run.id.clone(),
                repo_id: repo_id.clone(),
            });
            Ok((StatusCode::CREATED, Json(run)))
        }
        Err(e) => {
            let _ = agent_mgr.update_run_failed(&run.id, &e);
            Err(ConductorError::Agent(e).into())
        }
    }
}

/// List repo-scoped agent runs (newest first).
pub async fn list_repo_agent_runs(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<Vec<AgentRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let runs = mgr.list_repo_scoped(&repo_id)?;
    Ok(Json(runs))
}

/// Stop a repo-scoped agent run by run_id.
pub async fn stop_repo_agent(
    State(state): State<AppState>,
    Path((repo_id, run_id)): Path<(String, String)>,
) -> Result<Json<AgentRun>, ApiError> {
    let db = state.db.lock().await;
    let agent_mgr = AgentManager::new(&db);

    let run = agent_mgr
        .get_run(&run_id)?
        .ok_or_else(|| ConductorError::Agent("Agent run not found".to_string()))?;

    // Validate run belongs to the requested repo
    if run.repo_id.as_deref() != Some(&repo_id) {
        return Err(ConductorError::Agent("Agent run not found".to_string()).into());
    }

    if !run.is_active() {
        return Err(ConductorError::Agent("Agent is not running".to_string()).into());
    }

    cancel_agent_run(&agent_mgr, &run)?;

    let updated = agent_mgr.get_run(&run_id)?.unwrap_or(run);

    state.events.emit(ConductorEvent::RepoAgentStopped {
        run_id: updated.id.clone(),
        repo_id,
    });

    Ok(Json(updated))
}

/// Get events for a repo-scoped agent run.
pub async fn repo_agent_events(
    State(state): State<AppState>,
    Path((repo_id, run_id)): Path<(String, String)>,
) -> Result<Json<Vec<AgentEventResponse>>, ApiError> {
    let db = state.db.lock().await;
    let agent_mgr = AgentManager::new(&db);

    // Validate run belongs to the requested repo
    let run = agent_mgr
        .get_run(&run_id)?
        .ok_or_else(|| ConductorError::Agent("Agent run not found".to_string()))?;
    if run.repo_id.as_deref() != Some(&repo_id) {
        return Err(ConductorError::Agent("Agent run not found".to_string()).into());
    }

    let events: Vec<AgentEventResponse> = agent_mgr
        .list_events_for_run(&run_id)?
        .into_iter()
        .map(AgentEventResponse::from)
        .collect();

    Ok(Json(events))
}
