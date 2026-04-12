use std::borrow::Cow;
use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::agent::{
    parse_agent_log, AgentCreatedIssue, AgentEvent, AgentManager, AgentRun, AgentRunEvent,
    AgentRunStatus, FeedbackRequest, RunTreeTotals, TicketAgentTotals,
};
use conductor_core::error::ConductorError;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::{build_agent_prompt, TicketSyncer};
use conductor_core::worktree::WorktreeManager;

use tracing::warn;

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

/// Wire up PID persistence, drain thread, and panic monitor for a headless subprocess.
///
/// Shared lifecycle logic used by both [`spawn_headless_agent`] and
/// [`spawn_headless_orchestrate`]: persists the subprocess PID, spawns a
/// `spawn_blocking` drain thread that emits [`ConductorEvent::AgentLiveEvent`]
/// for each parsed event, and launches a panic-monitor task that marks the run
/// failed if the drain thread panics.
///
/// `prompt_file` is the temp file written by
/// [`conductor_core::agent_runtime::try_spawn_headless_run`]; pass `None` for
/// the orchestrate path which has no prompt file.
async fn wire_headless_drain(
    state: &AppState,
    run_id: &str,
    mut handle: conductor_core::agent_runtime::HeadlessHandle,
    prompt_file: Option<std::path::PathBuf>,
    worktree_id: Option<String>,
) -> Result<(), ApiError> {
    // Persist subprocess PID synchronously — stop_agent relies on this being visible
    // before any cancellation request arrives.
    let pid_result = {
        let db = state.db.lock().await;
        AgentManager::new(&db).update_run_subprocess_pid(run_id, handle.pid)
    };
    if let Err(e) = pid_result {
        // PID not persisted — stop_agent can't reach this process.
        // Kill the subprocess immediately and fail the run.
        let msg = format!("failed to persist subprocess pid: {e}");
        {
            let db = state.db.lock().await;
            if let Err(db_err) = AgentManager::new(&db).update_run_failed(run_id, &msg) {
                warn!(run_id, %db_err, "failed to mark run failed after PID persist error");
            }
        }
        if let Some(ref pf) = prompt_file {
            let _ = std::fs::remove_file(pf);
        }
        let _ = handle.child.kill();
        let _ = handle.child.wait();
        return Err(ConductorError::Agent(msg).into());
    }

    let run_id_owned = run_id.to_owned();
    let log_path = conductor_core::config::agent_log_path(&run_id_owned);
    let events = state.events.clone();
    let db_path = state.db_path.clone();
    let run_id_for_panic = run_id_owned.clone();
    let db_path_for_panic = db_path.clone();

    // Drain thread: reads stdout, persists events to DB, and emits AgentLiveEvent
    // on the SSE bus for connected browsers.
    let drain_handle = tokio::task::spawn_blocking(move || {
        let conn = match conductor_core::db::open_database(&db_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("[wire_headless_drain] drain: failed to open DB: {e}");
                if let Some(ref pf) = prompt_file {
                    let _ = std::fs::remove_file(pf);
                }
                // abort() drops the pipe read-ends before wait() to prevent a
                // deadlock where a child that has filled its stdout buffer can
                // never exit, so wait() would block forever.
                handle.abort();
                return;
            }
        };
        let mgr = AgentManager::new(&conn);
        conductor_core::agent_runtime::drain_stream_json(
            handle.stdout,
            &run_id_owned,
            &log_path,
            &mgr,
            |event| {
                events.emit(ConductorEvent::AgentLiveEvent {
                    run_id: run_id_owned.clone(),
                    worktree_id: worktree_id.clone(),
                    kind: event.kind.clone(),
                    summary: event.summary.clone(),
                });
            },
        );
        if let Some(ref pf) = prompt_file {
            let _ = std::fs::remove_file(pf);
        }
        let _ = {
            let mut c = handle.child;
            c.wait()
        };
    });

    // Monitor drain thread for panics — a panicking drain leaves the run permanently
    // stuck in 'running'. Catch it and mark the run failed.
    tokio::spawn(async move {
        if let Err(panic_err) = drain_handle.await {
            tracing::error!(
                run_id = %run_id_for_panic,
                "drain thread panicked: {panic_err}; marking run as failed"
            );
            match conductor_core::db::open_database(&db_path_for_panic) {
                Err(db_open_err) => {
                    tracing::error!(
                        run_id = %run_id_for_panic,
                        "drain panic handler: failed to open DB for recovery: {db_open_err}"
                    );
                }
                Ok(conn) => {
                    let mgr = AgentManager::new(&conn);
                    // Use update_run_failed_if_running to avoid clobbering a `completed`
                    // status written by drain_stream_json before the panic occurred (e.g.
                    // during the trailing remove_file / child.wait cleanup).
                    let msg = format!("drain thread panicked: {panic_err}");
                    if let Err(update_err) =
                        mgr.update_run_failed_if_running(&run_id_for_panic, &msg)
                    {
                        tracing::error!(
                            run_id = %run_id_for_panic,
                            "drain panic handler: failed to mark run as failed: {update_err}"
                        );
                    }
                }
            }
        }
    });

    Ok(())
}

/// Spawn a headless conductor subprocess and wire its stdout to the SSE event bus.
///
/// Calls [`conductor_core::agent_runtime::try_spawn_headless_run`], persists the
/// subprocess PID via [`wire_headless_drain`], then fires a
/// `tokio::task::spawn_blocking` drain thread (fire-and-forget) that emits
/// [`ConductorEvent::AgentLiveEvent`] for every event parsed from stdout.
#[allow(clippy::too_many_arguments)]
pub(super) async fn spawn_headless_agent(
    state: &AppState,
    run_id: &str,
    working_dir: &str,
    prompt: &str,
    resume_session_id: Option<&str>,
    model: Option<&str>,
    bot_name: Option<&str>,
    permission_mode: Option<&conductor_core::config::AgentPermissionMode>,
    worktree_id: Option<String>,
) -> Result<(), ApiError> {
    let spawn_result = conductor_core::agent_runtime::try_spawn_headless_run(
        run_id,
        working_dir,
        prompt,
        resume_session_id,
        model,
        bot_name,
        permission_mode,
        &[],
    );

    let (handle, prompt_file) = match spawn_result {
        Err(err) => {
            let db = state.db.lock().await;
            let agent_mgr = AgentManager::new(&db);
            if let Err(e) = agent_mgr.update_run_failed(run_id, &err) {
                warn!(run_id, %e, "failed to mark agent run as failed after headless spawn error");
            }
            return Err(ConductorError::Agent(err).into());
        }
        Ok(pair) => pair,
    };

    wire_headless_drain(state, run_id, handle, Some(prompt_file), worktree_id).await
}

/// Spawn a headless orchestrate subprocess and wire its stdout to the SSE event bus.
///
/// Calls [`conductor_core::agent_runtime::spawn_headless`] with pre-built args
/// from [`conductor_core::agent_runtime::build_orchestrate_args`], then delegates
/// PID persistence and drain wiring to [`wire_headless_drain`].
#[cfg(unix)]
async fn spawn_headless_orchestrate(
    state: &AppState,
    run_id: &str,
    args: Vec<Cow<'static, str>>,
    wt_path: String,
    worktree_id: String,
) -> Result<(), ApiError> {
    let handle = match conductor_core::agent_runtime::spawn_headless(
        &args,
        std::path::Path::new(&wt_path),
    ) {
        Err(err) => {
            let db = state.db.lock().await;
            if let Err(e) = AgentManager::new(&db).update_run_failed(run_id, &err) {
                warn!(run_id, %e, "failed to mark run failed after orchestrate spawn error");
            }
            return Err(ConductorError::Agent(err).into());
        }
        Ok(h) => h,
    };

    wire_headless_drain(state, run_id, handle, None, Some(worktree_id)).await
}

// ── Agent stats (aggregates) ──────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/agent-runs",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    responses(
        (status = 200, description = "List of agent runs for the worktree", body = Vec<AgentRun>),
    ),
    tag = "agents",
)]
pub async fn list_agent_runs(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<Vec<AgentRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let runs = mgr.list_for_worktree(&worktree_id)?;
    Ok(Json(runs))
}

#[derive(serde::Deserialize, utoipa::IntoParams)]
pub struct ListAllAgentRunsQuery {
    pub status: Option<String>,
}

/// List all agent runs globally, with optional `?status=` filter.
#[utoipa::path(
    get,
    path = "/api/agent/runs",
    params(ListAllAgentRunsQuery),
    responses(
        (status = 200, description = "List of all agent runs", body = Vec<AgentRun>),
    ),
    tag = "agents",
)]
pub async fn list_all_agent_runs(
    State(state): State<AppState>,
    Query(params): Query<ListAllAgentRunsQuery>,
) -> Result<Json<Vec<AgentRun>>, ApiError> {
    use std::str::FromStr;
    let status: Option<AgentRunStatus> = params
        .status
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| {
            AgentRunStatus::from_str(s).map_err(|e| ApiError::Core(ConductorError::InvalidInput(e)))
        })
        .transpose()?;

    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let runs = mgr.list_agent_runs(None, None, status.as_ref(), 500, 0)?;
    Ok(Json(runs))
}

#[utoipa::path(
    get,
    path = "/api/agent/latest-runs",
    responses(
        (status = 200, description = "Map of worktree ID to latest agent run"),
    ),
    tag = "agents",
)]
pub async fn latest_runs_by_worktree(
    State(state): State<AppState>,
) -> Result<Json<HashMap<String, AgentRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let map = mgr.latest_runs_by_worktree()?;
    Ok(Json(map))
}

#[utoipa::path(
    get,
    path = "/api/agent/ticket-totals",
    responses(
        (status = 200, description = "Map of ticket ID to agent totals"),
    ),
    tag = "agents",
)]
pub async fn ticket_totals(
    State(state): State<AppState>,
) -> Result<Json<HashMap<String, TicketAgentTotals>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let map = mgr.totals_by_ticket_all()?;
    Ok(Json(map))
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/agent/latest-runs",
    params(
        ("id" = String, Path, description = "Repo ID"),
    ),
    responses(
        (status = 200, description = "Map of worktree ID to latest agent run for repo"),
    ),
    tag = "agents",
)]
pub async fn latest_runs_by_worktree_for_repo(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<HashMap<String, AgentRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let map = mgr.latest_runs_by_worktree_for_repo(&repo_id)?;
    Ok(Json(map))
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/agent/ticket-totals",
    params(
        ("id" = String, Path, description = "Repo ID"),
    ),
    responses(
        (status = 200, description = "Map of ticket ID to agent totals for repo"),
    ),
    tag = "agents",
)]
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
#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/agent/runs",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    responses(
        (status = 200, description = "List of agent runs for worktree", body = Vec<AgentRun>),
    ),
    tag = "agents",
)]
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
#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/agent/latest",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    responses(
        (status = 200, description = "Latest agent run or null", body = Option<AgentRun>),
    ),
    tag = "agents",
)]
pub async fn latest_run(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<Option<AgentRun>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let run = mgr.latest_for_worktree(&worktree_id)?;
    Ok(Json(run))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct StartAgentRequest {
    pub prompt: String,
    pub resume_session_id: Option<String>,
    pub parent_run_id: Option<String>,
}

/// Start an agent for a worktree. Creates a DB record and spawns a headless subprocess.
#[utoipa::path(
    post,
    path = "/api/worktrees/{id}/agent/start",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    request_body(content = StartAgentRequest, description = "Agent start parameters"),
    responses(
        (status = 201, description = "Agent run created", body = AgentRun),
        (status = 404, description = "Worktree not found"),
    ),
    tag = "agents",
)]
pub async fn start_agent(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
    Json(body): Json<StartAgentRequest>,
) -> Result<(StatusCode, Json<AgentRun>), ApiError> {
    // Scope DB + config access so locks are dropped before the blocking spawn.
    let (run, wt_path, wt_id, prompt, resume_session_id, model) = {
        let db = state.db.lock().await;
        let config = state.config.read().await;

        // Look up the worktree to get slug and path
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
                None,
                model.as_deref(),
                parent_id,
                None,
            )?
        } else {
            agent_mgr.create_run(Some(&worktree_id), &body.prompt, None, model.as_deref())?
        };

        (
            run,
            wt.path.clone(),
            wt.id.clone(),
            body.prompt.clone(),
            body.resume_session_id.clone(),
            model,
        )
    };
    // DB and config locks are now dropped.

    // Spawn headless subprocess and wire stdout to the SSE event bus.
    spawn_headless_agent(
        &state,
        &run.id,
        &wt_path,
        &prompt,
        resume_session_id.as_deref(),
        model.as_deref(),
        None,
        None,
        Some(wt_id.clone()),
    )
    .await?;

    state.events.emit(ConductorEvent::AgentStarted {
        run_id: run.id.clone(),
        worktree_id: wt_id,
    });
    Ok((StatusCode::CREATED, Json(run)))
}

/// Stop a running agent: mark cancelled under lock, then signal the subprocess
/// on a blocking thread without holding the DB mutex.
#[utoipa::path(
    post,
    path = "/api/worktrees/{id}/agent/stop",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    responses(
        (status = 200, description = "Stopped agent run", body = AgentRun),
        (status = 404, description = "Worktree or active agent not found"),
    ),
    tag = "agents",
)]
pub async fn stop_agent(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
) -> Result<Json<AgentRun>, ApiError> {
    // Phase 1: DB read under lock — validate only, no writes.
    let (run_id, subprocess_pid) = {
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

        (run.id, run.subprocess_pid)
    };
    // DB lock is now dropped.

    // Phase 2: cancel via AgentManager::cancel_run() on a blocking thread (no lock held).
    // cancel_run() marks the DB cancelled first, then best-effort kills the subprocess.
    let db_path = state.db_path.clone();
    let run_id_clone = run_id.clone();
    tokio::task::spawn_blocking(move || {
        let conn = conductor_core::db::open_database(&db_path)?;
        AgentManager::new(&conn).cancel_run(&run_id_clone, subprocess_pid)
    })
    .await
    .map_err(|e| {
        warn!(run_id = %run_id, %e, "stop_agent: cancel task panicked");
        ConductorError::Agent(format!("cancel task panicked: {e}"))
    })??;

    // Re-fetch under lock to return the updated record.
    let updated = {
        let db = state.db.lock().await;
        let agent_mgr = AgentManager::new(&db);
        agent_mgr
            .latest_for_worktree(&worktree_id)?
            .ok_or_else(|| {
                ConductorError::Agent("No agent run found for this worktree".to_string())
            })?
    };

    state.events.emit(ConductorEvent::AgentStopped {
        run_id: updated.id.clone(),
        worktree_id: worktree_id.clone(),
    });

    Ok(Json(updated))
}

#[derive(Serialize, utoipa::ToSchema)]
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
#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/agent/events",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    responses(
        (status = 200, description = "List of agent events", body = Vec<AgentEventResponse>),
    ),
    tag = "agents",
)]
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
#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/agent/runs/{run_id}/events",
    params(
        ("id" = String, Path, description = "Worktree ID"),
        ("run_id" = String, Path, description = "Agent run ID"),
    ),
    responses(
        (status = 200, description = "List of events for the agent run", body = Vec<AgentEventResponse>),
    ),
    tag = "agents",
)]
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct AgentPromptResponse {
    pub prompt: String,
    pub resume_session_id: Option<String>,
    /// True if the latest run ended with incomplete plan steps and can be auto-resumed.
    pub needs_resume: bool,
    /// Number of incomplete plan steps remaining (0 if no resume needed).
    pub incomplete_steps: usize,
}

/// Get a pre-filled agent prompt for a worktree (from its linked ticket).
#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/agent/prompt",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    responses(
        (status = 200, description = "Agent prompt for worktree", body = AgentPromptResponse),
        (status = 404, description = "Worktree not found"),
    ),
    tag = "agents",
)]
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
#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/agent/runs/{run_id}/children",
    params(
        ("id" = String, Path, description = "Worktree ID"),
        ("run_id" = String, Path, description = "Parent agent run ID"),
    ),
    responses(
        (status = 200, description = "List of child agent runs", body = Vec<AgentRun>),
    ),
    tag = "agents",
)]
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
#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/agent/runs/{run_id}/tree",
    params(
        ("id" = String, Path, description = "Worktree ID"),
        ("run_id" = String, Path, description = "Root agent run ID"),
    ),
    responses(
        (status = 200, description = "Full run tree (root + all descendants)", body = Vec<AgentRun>),
    ),
    tag = "agents",
)]
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
#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/agent/runs/{run_id}/tree-totals",
    params(
        ("id" = String, Path, description = "Worktree ID"),
        ("run_id" = String, Path, description = "Root agent run ID"),
    ),
    responses(
        (status = 200, description = "Aggregated totals for a run tree", body = RunTreeTotals),
    ),
    tag = "agents",
)]
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
#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/agent/created-issues",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    responses(
        (status = 200, description = "List of GitHub issues created by agent runs", body = Vec<AgentCreatedIssue>),
    ),
    tag = "agents",
)]
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

#[derive(Deserialize, utoipa::ToSchema)]
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
/// for each step sequentially. The orchestrator runs headless.
#[utoipa::path(
    post,
    path = "/api/worktrees/{id}/agent/orchestrate",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    request_body(content = OrchestrateRequest, description = "Orchestration parameters"),
    responses(
        (status = 201, description = "Orchestrator agent run created", body = AgentRun),
        (status = 404, description = "Worktree not found"),
    ),
    tag = "agents",
)]
pub async fn orchestrate_agent(
    State(state): State<AppState>,
    Path(worktree_id): Path<String>,
    Json(body): Json<OrchestrateRequest>,
) -> Result<(StatusCode, Json<AgentRun>), ApiError> {
    // Scope DB + config access so locks are dropped before the blocking spawn.
    let (run, args, wt_path, wt_id) = {
        let db = state.db.lock().await;
        let config = state.config.read().await;

        // Look up the worktree
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
        let run = agent_mgr.create_run(Some(&worktree_id), &body.prompt, None, model.as_deref())?;

        // Build conductor agent orchestrate command
        let args = conductor_core::agent_runtime::build_orchestrate_args(
            &run.id,
            &wt.path,
            model.as_deref(),
            body.fail_fast,
            Some(body.child_timeout_secs),
        );

        (run, args, wt.path.clone(), wt.id.clone())
    };
    // DB and config locks are now dropped.

    // Spawn headless subprocess and wire stdout to the SSE event bus.
    spawn_headless_orchestrate(&state, &run.id, args, wt_path, wt_id.clone()).await?;

    state.events.emit(ConductorEvent::AgentStarted {
        run_id: run.id.clone(),
        worktree_id: wt_id,
    });
    Ok((StatusCode::CREATED, Json(run)))
}

// ── Feedback (human-in-the-loop) ──────────────────────────────────────

/// Get the pending feedback request for a worktree's running agent (if any).
#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/agent/feedback",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    responses(
        (status = 200, description = "Pending feedback request or null", body = Option<FeedbackRequest>),
    ),
    tag = "agents",
)]
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
#[utoipa::path(
    get,
    path = "/api/worktrees/{id}/agent/runs/{run_id}/feedback",
    params(
        ("id" = String, Path, description = "Worktree ID"),
        ("run_id" = String, Path, description = "Agent run ID"),
    ),
    responses(
        (status = 200, description = "List of feedback requests for the run", body = Vec<FeedbackRequest>),
    ),
    tag = "agents",
)]
pub async fn list_run_feedback(
    State(state): State<AppState>,
    Path((_worktree_id, run_id)): Path<(String, String)>,
) -> Result<Json<Vec<FeedbackRequest>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let feedback = mgr.list_feedback_for_run(&run_id)?;
    Ok(Json(feedback))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RequestFeedbackBody {
    pub prompt: String,
}

/// Create a feedback request for a running agent (pauses the agent).
#[utoipa::path(
    post,
    path = "/api/worktrees/{id}/agent/feedback",
    params(
        ("id" = String, Path, description = "Worktree ID"),
    ),
    request_body(content = RequestFeedbackBody, description = "Feedback prompt"),
    responses(
        (status = 201, description = "Feedback request created", body = FeedbackRequest),
        (status = 404, description = "Worktree or active agent not found"),
    ),
    tag = "agents",
)]
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

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SubmitFeedbackBody {
    pub response: String,
}

/// Submit a response to a pending feedback request (resumes the agent).
#[utoipa::path(
    post,
    path = "/api/worktrees/{id}/agent/feedback/{feedback_id}/respond",
    params(
        ("id" = String, Path, description = "Worktree ID"),
        ("feedback_id" = String, Path, description = "Feedback request ID"),
    ),
    request_body(content = SubmitFeedbackBody, description = "Feedback response"),
    responses(
        (status = 200, description = "Updated feedback request", body = FeedbackRequest),
        (status = 404, description = "Feedback request not found"),
    ),
    tag = "agents",
)]
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
#[utoipa::path(
    post,
    path = "/api/worktrees/{id}/agent/feedback/{feedback_id}/dismiss",
    params(
        ("id" = String, Path, description = "Worktree ID"),
        ("feedback_id" = String, Path, description = "Feedback request ID"),
    ),
    responses(
        (status = 204, description = "Feedback dismissed"),
        (status = 404, description = "Feedback request not found"),
    ),
    tag = "agents",
)]
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
    let fb = mgr.get_feedback(feedback_id)?.ok_or_else(|| {
        ApiError::Core(ConductorError::FeedbackNotFound {
            id: feedback_id.to_string(),
        })
    })?;
    let run = mgr.get_run(&fb.run_id)?.ok_or_else(|| {
        ApiError::Core(ConductorError::AgentRunNotFound {
            id: fb.run_id.clone(),
        })
    })?;
    if run.worktree_id.as_deref() != Some(worktree_id) {
        return Err(ApiError::Core(ConductorError::Agent(
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
/// same prompt/config and re-spawning a headless subprocess.
#[utoipa::path(
    post,
    path = "/api/worktrees/{id}/agent/runs/{run_id}/restart",
    params(
        ("id" = String, Path, description = "Worktree ID"),
        ("run_id" = String, Path, description = "Agent run ID to restart"),
    ),
    responses(
        (status = 201, description = "New agent run created from restart", body = AgentRun),
        (status = 404, description = "Worktree or run not found"),
    ),
    tag = "agents",
)]
pub async fn restart_agent(
    State(state): State<AppState>,
    Path((worktree_id, run_id)): Path<(String, String)>,
) -> Result<(StatusCode, Json<AgentRun>), ApiError> {
    // Scope DB + config access so locks are dropped before the blocking spawn.
    let (new_run, wt_path) = {
        let db = state.db.lock().await;
        let config = state.config.read().await;

        let agent_mgr = AgentManager::new(&db);

        // Validate ownership BEFORE creating the child run (IDOR guard).
        let original = agent_mgr
            .get_run(&run_id)?
            .ok_or_else(|| ConductorError::Agent(format!("Run {run_id} not found")))?;
        if original.worktree_id.as_deref() != Some(worktree_id.as_str()) {
            return Err(ConductorError::Agent(
                "run does not belong to the specified worktree".to_string(),
            )
            .into());
        }

        let new_run = agent_mgr.restart_run(&run_id)?;

        // Resolve worktree path from new_run.worktree_id (single source of truth).
        let wt_mgr = WorktreeManager::new(&db, &config);
        let wt = wt_mgr.get_by_id(
            new_run
                .worktree_id
                .as_deref()
                .expect("worktree_id verified above"),
        )?;

        (new_run, wt.path.clone())
    };
    // DB and config locks are now dropped.

    // Spawn headless subprocess and wire stdout to the SSE event bus.
    spawn_headless_agent(
        &state,
        &new_run.id,
        &wt_path,
        &new_run.prompt,
        None,
        new_run.model.as_deref(),
        new_run.bot_name.as_deref(),
        None,
        Some(worktree_id.clone()),
    )
    .await?;

    state.events.emit(ConductorEvent::AgentRestarted {
        run_id: new_run.id.clone(),
        old_run_id: run_id,
        worktree_id: worktree_id.clone(),
    });
    Ok((StatusCode::CREATED, Json(new_run)))
}

// ── Repo-scoped agent routes ────────────────────────────────────────────

#[derive(Deserialize, utoipa::ToSchema)]
pub struct StartRepoAgentRequest {
    pub prompt: String,
    /// When true, ignore any prior session and start fresh.
    #[serde(default)]
    pub new_session: bool,
}

/// Start a read-only agent scoped to a repo. Uses `--allowedTools` restriction with
/// `--dangerously-skip-permissions` for unrestricted Bash/gh access while blocking
/// file-writing tools (Edit, Write, MultiEdit, NotebookEdit).
#[utoipa::path(
    post,
    path = "/api/repos/{id}/agent/start",
    params(
        ("id" = String, Path, description = "Repo ID"),
    ),
    request_body(content = StartRepoAgentRequest, description = "Repo agent start parameters"),
    responses(
        (status = 201, description = "Repo-scoped agent run created", body = AgentRun),
        (status = 404, description = "Repo not found"),
    ),
    tag = "agents",
)]
pub async fn start_repo_agent(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    Json(body): Json<StartRepoAgentRequest>,
) -> Result<(StatusCode, Json<AgentRun>), ApiError> {
    // Scope DB + config access so locks are dropped before the blocking spawn.
    let (run, repo_path, resume_session_id, model) = {
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

        let run = agent_mgr.create_repo_run(&repo_id, &body.prompt, None, model.as_deref())?;

        (run, repo.local_path.clone(), resume_session_id, model)
    };
    // DB and config locks are now dropped.

    // Spawn headless subprocess with repo-safe permission mode and wire stdout to SSE.
    let repo_safe = conductor_core::config::AgentPermissionMode::RepoSafe;
    spawn_headless_agent(
        &state,
        &run.id,
        &repo_path,
        &run.prompt,
        resume_session_id.as_deref(),
        model.as_deref(),
        None,
        Some(&repo_safe),
        None,
    )
    .await?;

    state.events.emit(ConductorEvent::RepoAgentStarted {
        run_id: run.id.clone(),
        repo_id: repo_id.clone(),
    });
    Ok((StatusCode::CREATED, Json(run)))
}

/// List repo-scoped agent runs (newest first).
#[utoipa::path(
    get,
    path = "/api/repos/{id}/agent/runs",
    params(
        ("id" = String, Path, description = "Repo ID"),
    ),
    responses(
        (status = 200, description = "List of repo-scoped agent runs", body = Vec<AgentRun>),
    ),
    tag = "agents",
)]
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
#[utoipa::path(
    post,
    path = "/api/repos/{id}/agent/{run_id}/stop",
    params(
        ("id" = String, Path, description = "Repo ID"),
        ("run_id" = String, Path, description = "Agent run ID"),
    ),
    responses(
        (status = 200, description = "Stopped agent run", body = AgentRun),
        (status = 404, description = "Repo or run not found"),
    ),
    tag = "agents",
)]
pub async fn stop_repo_agent(
    State(state): State<AppState>,
    Path((repo_id, run_id)): Path<(String, String)>,
) -> Result<Json<AgentRun>, ApiError> {
    // Phase 1: DB read under lock — validate only, no writes.
    let subprocess_pid = {
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

        run.subprocess_pid
    };
    // DB lock is now dropped.

    // Phase 2: cancel via AgentManager::cancel_run() on a blocking thread (no lock held).
    // cancel_run() marks the DB cancelled first, then best-effort kills the subprocess.
    let db_path = state.db_path.clone();
    let run_id_clone = run_id.clone();
    tokio::task::spawn_blocking(move || {
        let conn = conductor_core::db::open_database(&db_path)?;
        AgentManager::new(&conn).cancel_run(&run_id_clone, subprocess_pid)
    })
    .await
    .map_err(|e| {
        warn!(run_id = %run_id, %e, "stop_repo_agent: cancel task panicked");
        ConductorError::Agent(format!("cancel task panicked: {e}"))
    })??;

    // Re-fetch under lock to return the updated record.
    let updated = {
        let db = state.db.lock().await;
        let agent_mgr = AgentManager::new(&db);
        agent_mgr
            .get_run(&run_id)?
            .ok_or_else(|| ConductorError::Agent("Agent run not found".to_string()))?
    };

    state.events.emit(ConductorEvent::RepoAgentStopped {
        run_id: updated.id.clone(),
        repo_id,
    });

    Ok(Json(updated))
}

/// Get events for a repo-scoped agent run.
#[utoipa::path(
    get,
    path = "/api/repos/{id}/agent/{run_id}/events",
    params(
        ("id" = String, Path, description = "Repo ID"),
        ("run_id" = String, Path, description = "Agent run ID"),
    ),
    responses(
        (status = 200, description = "List of events for repo-scoped agent run", body = Vec<AgentEventResponse>),
        (status = 404, description = "Repo or run not found"),
    ),
    tag = "agents",
)]
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

// ── Global agent run endpoints ────────────────────────────────────────

/// Get a single agent run by its ID (globally scoped, no worktree required).
#[utoipa::path(
    get,
    path = "/api/agent/runs/{id}",
    params(
        ("id" = String, Path, description = "Agent run ID"),
    ),
    responses(
        (status = 200, description = "Agent run", body = AgentRun),
        (status = 404, description = "Agent run not found"),
    ),
    tag = "agents",
)]
pub async fn get_agent_run_by_id(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<AgentRun>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let run = mgr.get_run(&run_id)?.ok_or_else(|| {
        ApiError::Core(ConductorError::Agent(format!(
            "agent run {run_id} not found"
        )))
    })?;
    Ok(Json(run))
}

/// List all feedback requests for a given agent run ID (globally scoped).
#[utoipa::path(
    get,
    path = "/api/agent/runs/{id}/feedback",
    params(
        ("id" = String, Path, description = "Agent run ID"),
    ),
    responses(
        (status = 200, description = "List of feedback requests for the run", body = Vec<FeedbackRequest>),
    ),
    tag = "agents",
)]
pub async fn get_agent_run_feedback_by_run_id(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<Vec<FeedbackRequest>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);
    let feedback = mgr.list_feedback_for_run(&run_id)?;
    Ok(Json(feedback))
}

/// Get parsed agent events for a single run by ID — scope-agnostic.
///
/// Checks DB-persisted events first; falls back to log-file parsing for older runs.
#[utoipa::path(
    get,
    path = "/api/agent/runs/{id}/events",
    params(
        ("id" = String, Path, description = "Agent run ID"),
    ),
    responses(
        (status = 200, description = "List of events for the agent run", body = Vec<AgentEventResponse>),
    ),
    tag = "agents",
)]
pub async fn get_agent_run_events_by_id(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<Vec<AgentEventResponse>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = AgentManager::new(&db);

    let db_events = mgr.list_events_for_run(&run_id)?;
    if !db_events.is_empty() {
        return Ok(Json(
            db_events
                .into_iter()
                .map(AgentEventResponse::from)
                .collect(),
        ));
    }

    // Fall back to log-file parsing for runs without persisted DB events.
    let events = mgr
        .get_run(&run_id)?
        .and_then(|r| r.log_file)
        .map(|path| {
            parse_agent_log(&path)
                .into_iter()
                .map(AgentEventResponse::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(Json(events))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tokio::sync::{Mutex, RwLock};
    use tower::ServiceExt;

    use conductor_core::agent::{AgentManager, AgentRunStatus};
    use conductor_core::config::Config;

    use crate::events::EventBus;
    use crate::routes::api_router;
    use crate::state::AppState;
    use crate::test_helpers::seeded_state;

    /// Verify that when `try_spawn_headless_run` fails (working dir does not exist,
    /// or the `conductor` binary is not on PATH), the route:
    ///   1. Returns a non-201 error response.
    ///   2. Marks the newly-created agent run as `failed` in the DB.
    #[tokio::test]
    async fn start_agent_spawn_failure_marks_run_failed() {
        let (state, _tmp) = seeded_state();

        // Insert a worktree whose path is guaranteed not to exist so that
        // `spawn_headless` fails with an OS error and exercises the error path.
        {
            let db = state.db.lock().await;
            conductor_core::test_helpers::insert_test_worktree(
                &db,
                "w-bad",
                "r1",
                "bad-path-wt",
                "/totally/nonexistent/conductor/test/path",
            );
        }

        let app = api_router().with_state(state.clone());
        let body = serde_json::json!({ "prompt": "do something" }).to_string();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/worktrees/w-bad/agent/start")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        // spawn failure → ConductorError::Agent → 400 Bad Request
        assert_ne!(
            response.status(),
            StatusCode::CREATED,
            "expected an error response when spawn fails"
        );

        // The run created inside start_agent must be marked failed.
        let db = state.db.lock().await;
        let mgr = AgentManager::new(&db);
        let runs = mgr.list_for_worktree("w-bad").unwrap();
        assert_eq!(
            runs.len(),
            1,
            "expected exactly one agent run to be created"
        );
        assert_eq!(
            runs[0].status,
            AgentRunStatus::Failed,
            "run should be marked failed after spawn error"
        );
    }

    /// Verify the DB-level operations in the PID-persist failure path (lines 106–120).
    ///
    /// When `update_run_subprocess_pid` fails, `spawn_headless_agent` calls
    /// `update_run_failed` and returns an error so the run is never stuck in `running`.
    ///
    /// Note: the full HTTP-layer integration path cannot be exercised in unit tests
    /// because it requires a live `conductor` subprocess to spawn (the PID-persist step
    /// only runs after a successful spawn). This test covers the DB state transition the
    /// failure path relies on.
    #[tokio::test]
    async fn pid_persist_failure_path_marks_run_failed() {
        let (state, _tmp) = seeded_state();
        let run_id = "pid-persist-fail-run";

        // Simulate: a run was created in 'running' state (as start_agent does).
        {
            let db = state.db.lock().await;
            conductor_core::test_helpers::insert_test_agent_run(&db, run_id, "w1");
        }

        // Simulate what the PID-persist failure path does: call update_run_failed with
        // the same message format to prevent the run from being stuck in 'running'.
        let pid_err = "disk I/O error";
        let msg = format!("failed to persist subprocess pid: {pid_err}");
        {
            let db = state.db.lock().await;
            AgentManager::new(&db)
                .update_run_failed(run_id, &msg)
                .expect("update_run_failed must succeed");
        }

        // The run must now be failed, not stuck in 'running'.
        let db = state.db.lock().await;
        let run = AgentManager::new(&db)
            .get_run(run_id)
            .unwrap()
            .expect("run must exist");
        assert_eq!(run.status, AgentRunStatus::Failed, "run should be failed");
        assert!(
            run.result_text
                .as_deref()
                .unwrap_or("")
                .contains("persist subprocess pid"),
            "result_text should reference 'persist subprocess pid', got: {:?}",
            run.result_text
        );
    }

    /// Verify that the drain-panic monitor does NOT clobber a `completed` run.
    ///
    /// `update_run_failed_if_running` is used in the panic handler so that if
    /// `drain_stream_json` already finalized the run (e.g. `completed`) before the
    /// drain thread panicked in the trailing cleanup, the `completed` status is preserved.
    #[tokio::test]
    async fn drain_panic_monitor_does_not_clobber_completed_run() {
        let (state, _tmp) = seeded_state();
        let run_id = "drain-panic-completed-run";

        // Seed a run, then mark it completed (simulating drain_stream_json success).
        {
            let db = state.db.lock().await;
            conductor_core::test_helpers::insert_test_agent_run(&db, run_id, "w1");
            let mgr = AgentManager::new(&db);
            // Mark completed so the run is no longer 'running'.
            mgr.update_run_completed_if_running(run_id, "done")
                .expect("update_run_completed_if_running must succeed");
        }

        // Simulate what the panic monitor does: update_run_failed_if_running.
        // Because the run is already 'completed', this must be a no-op.
        {
            let db = state.db.lock().await;
            AgentManager::new(&db)
                .update_run_failed_if_running(run_id, "drain thread panicked")
                .expect("update_run_failed_if_running must not return an error");
        }

        // Status must still be 'completed' — not overwritten by the panic handler.
        let db = state.db.lock().await;
        let run = AgentManager::new(&db)
            .get_run(run_id)
            .unwrap()
            .expect("run must exist");
        assert_eq!(
            run.status,
            AgentRunStatus::Completed,
            "completed run must not be clobbered by drain panic handler"
        );
    }

    /// Regression test: drain-thread DB-open failure must not deadlock.
    ///
    /// **Bug (pre-fix):** `handle.child.wait()` was called while `handle.stdout`
    /// was still open.  The test child writes 128 KiB — more than the 64 KiB
    /// default pipe buffer — so it blocks after filling the buffer.  `wait()`
    /// then blocks waiting for the child to exit, and neither makes progress.
    ///
    /// **Fix:** `stdout` and `stderr` are dropped before `wait()` so the child
    /// receives EPIPE and exits; `wait()` returns immediately.
    ///
    /// The test gives the background drain task 5 seconds to complete.  Without
    /// the fix it would hang the full 5 seconds and then panic.
    #[cfg(unix)]
    #[tokio::test]
    async fn drain_db_open_failure_no_pipe_deadlock() {
        use std::process::{Command, Stdio};

        // Spawn a process that writes 128 KiB to stdout — enough to fill the
        // 64 KiB pipe buffer and leave the child blocked on its next write if
        // the read end is not closed before wait().
        let child = Command::new("sh")
            .args(["-c", "dd if=/dev/zero bs=131072 count=1 2>/dev/null"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn dd writer");

        let child_pid = child.id();
        let handle = conductor_core::agent_runtime::HeadlessHandle::from_child(child)
            .expect("HeadlessHandle::from_child");

        // Build an AppState with a valid shared DB (so update_run_subprocess_pid
        // succeeds) but a non-existent db_path (so the drain thread's secondary
        // DB open fails and exercises the failure branch).
        let tmp = tempfile::NamedTempFile::new().expect("temp db");
        let conn = conductor_core::db::open_database(tmp.path()).expect("open db");
        conductor_core::test_helpers::insert_test_repo(&conn, "r1", "test-repo", "/tmp/repo");
        conductor_core::test_helpers::insert_test_worktree(
            &conn,
            "w1",
            "r1",
            "feat-test",
            "/tmp/ws/feat-test",
        );
        let run = AgentManager::new(&conn)
            .create_run(Some("w1"), "test prompt", None, None)
            .expect("create run");
        let run_id = run.id.clone();
        let state = AppState {
            db: Arc::new(Mutex::new(conn)),
            config: Arc::new(RwLock::new(Config::default())),
            events: EventBus::new(8),
            // Deliberately bad path so the drain thread's DB open fails.
            db_path: std::path::PathBuf::from("/nonexistent/__conductor_drain_test.db"),
            workflow_done_notify: None,
        };

        // wire_headless_drain returns Ok quickly (persists PID, spawns tasks).
        super::wire_headless_drain(&state, &run_id, handle, None, None)
            .await
            .expect("wire_headless_drain should return Ok");

        // Poll until the child process disappears from the process table.
        // `ps -p <pid>` exits 0 while the process exists (running or zombie),
        // non-zero once it has been fully reaped by wait().
        // Without the fix the child fills its pipe and neither wait() nor the
        // child can make progress — this loop would time out.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let alive = Command::new("ps")
                .args(["-p", &child_pid.to_string()])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !alive {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child PID {child_pid} still in process table after 5 s \
                 — likely pipe-buffer deadlock in drain thread (stdout/stderr \
                 not dropped before wait())"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Keep _tmp alive until the DB is no longer needed.
        let _ = tmp;
    }
}
