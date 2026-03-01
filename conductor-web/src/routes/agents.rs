use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::Json;

use conductor_core::agent::{AgentManager, AgentRun, TicketAgentTotals};

use crate::error::ApiError;
use crate::state::AppState;

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
