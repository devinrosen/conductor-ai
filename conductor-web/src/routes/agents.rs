use std::collections::HashMap;
use std::process::Command;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::agent::{parse_agent_log, AgentEvent, AgentManager, AgentRun, TicketAgentTotals};
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
}

/// Start an agent for a worktree. Creates a DB record and spawns a tmux window.
pub async fn start_agent(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
    Json(body): Json<StartAgentRequest>,
) -> Result<(StatusCode, Json<AgentRun>), ApiError> {
    let db = state.db.lock().await;

    // Look up the worktree to get slug and path
    let wt_mgr = WorktreeManager::new(&db, &state.config);
    let wt = wt_mgr.get_by_id(&worktree_id)?;

    // Check if there's already a running agent
    let agent_mgr = AgentManager::new(&db);
    if let Some(existing) = agent_mgr.latest_for_worktree(&worktree_id)? {
        if existing.status == "running" {
            return Err(conductor_core::error::ConductorError::Agent(
                "Agent already running for this worktree".to_string(),
            )
            .into());
        }
    }

    // Create DB record
    let run = agent_mgr.create_run(&worktree_id, &body.prompt, Some(&wt.slug))?;

    // Build conductor agent run command
    let mut args = vec![
        "agent".to_string(),
        "run".to_string(),
        "--run-id".to_string(),
        run.id.clone(),
        "--worktree-path".to_string(),
        wt.path.clone(),
        "--prompt".to_string(),
        body.prompt,
    ];

    if let Some(ref session_id) = body.resume_session_id {
        args.push("--resume".to_string());
        args.push(session_id.clone());
    }

    // Resolve conductor binary
    let conductor_bin = std::env::current_exe()
        .ok()
        .and_then(|p| {
            let sibling = p.parent()?.join("conductor");
            sibling
                .exists()
                .then(|| sibling.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "conductor".to_string());

    // Spawn tmux window
    let mut tmux_args = vec![
        "new-window".to_string(),
        "-d".to_string(),
        "-n".to_string(),
        wt.slug.clone(),
        "--".to_string(),
        conductor_bin,
    ];
    tmux_args.extend(args);

    let result = Command::new("tmux").args(&tmux_args).output();

    match result {
        Ok(o) if o.status.success() => {
            state.events.emit(ConductorEvent::AgentStarted {
                run_id: run.id.clone(),
                worktree_id: wt.id.clone(),
            });
            Ok((StatusCode::CREATED, Json(run)))
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let _ = agent_mgr.update_run_failed(&run.id, &format!("tmux failed: {stderr}"));
            Err(conductor_core::error::ConductorError::Agent(format!(
                "Failed to spawn tmux window: {stderr}"
            ))
            .into())
        }
        Err(e) => {
            let _ = agent_mgr.update_run_failed(&run.id, &format!("tmux error: {e}"));
            Err(
                conductor_core::error::ConductorError::Agent(format!("Failed to spawn tmux: {e}"))
                    .into(),
            )
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

    if run.status != "running" {
        return Err(conductor_core::error::ConductorError::Agent(
            "Agent is not running".to_string(),
        )
        .into());
    }

    // Capture tmux scrollback before killing
    if let Some(ref window) = run.tmux_window {
        capture_agent_log(&agent_mgr, &run.id, window);
    }

    // Kill tmux window
    if let Some(ref window) = run.tmux_window {
        let _ = Command::new("tmux")
            .args(["kill-window", "-t", &format!(":{window}")])
            .output();
    }

    // Mark as cancelled
    agent_mgr.update_run_cancelled(&run.id)?;

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
    pub kind: String,
    pub summary: String,
}

impl From<AgentEvent> for AgentEventResponse {
    fn from(e: AgentEvent) -> Self {
        Self {
            kind: e.kind,
            summary: e.summary,
        }
    }
}

/// Get parsed agent events from the log file of the latest run.
pub async fn get_events(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<Vec<AgentEventResponse>>, ApiError> {
    let db = state.db.lock().await;
    let agent_mgr = AgentManager::new(&db);

    let runs = agent_mgr.list_for_worktree(&worktree_id)?;
    let mut all_events = Vec::new();

    for run in runs.iter().rev() {
        if let Some(ref log_file) = run.log_file {
            let events = parse_agent_log(log_file);
            all_events.extend(events.into_iter().map(AgentEventResponse::from));
        }
    }

    Ok(Json(all_events))
}

#[derive(Serialize)]
pub struct AgentPromptResponse {
    pub prompt: String,
    pub resume_session_id: Option<String>,
}

/// Get a pre-filled agent prompt for a worktree (from its linked ticket).
pub async fn get_prompt(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<AgentPromptResponse>, ApiError> {
    let db = state.db.lock().await;

    // Look up worktree to get ticket_id
    let wt_mgr = WorktreeManager::new(&db, &state.config);
    let wt = wt_mgr.get_by_id(&worktree_id)?;

    // Build prompt from ticket if linked
    let prompt = if let Some(ref ticket_id) = wt.ticket_id {
        let syncer = TicketSyncer::new(&db);
        match syncer.get_by_id(ticket_id) {
            Ok(ticket) => build_agent_prompt(&ticket),
            Err(_) => String::new(),
        }
    } else {
        String::new()
    };

    // Check for resumable session
    let agent_mgr = AgentManager::new(&db);
    let resume_session_id = agent_mgr
        .latest_for_worktree(&wt.id)?
        .and_then(|run| run.claude_session_id);

    Ok(Json(AgentPromptResponse {
        prompt,
        resume_session_id,
    }))
}

/// Best-effort capture of tmux scrollback to `~/.conductor/agent-logs/<run_id>.log`.
fn capture_agent_log(mgr: &AgentManager, run_id: &str, tmux_window: &str) {
    let log_dir = conductor_core::config::conductor_dir().join("agent-logs");

    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        tracing::warn!("could not create agent-logs dir: {e}");
        return;
    }

    let log_path = log_dir.join(format!("{run_id}.log"));

    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-t",
            &format!(":{tmux_window}"),
            "-p",
            "-S",
            "-",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            if let Err(e) = std::fs::write(&log_path, &o.stdout) {
                tracing::warn!("could not write agent log: {e}");
                return;
            }
            let path_str = log_path.to_string_lossy().to_string();
            let _ = mgr.update_run_log_file(run_id, &path_str);
        }
        _ => {}
    }
}
