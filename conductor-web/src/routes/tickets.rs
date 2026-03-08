use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use conductor_core::agent::{AgentManager, TicketAgentTotals};
use conductor_core::github;
use conductor_core::issue_source::{GitHubConfig, IssueSourceManager, JiraConfig};
use conductor_core::jira_acli;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::{Ticket, TicketSyncer};
use conductor_core::worktree::{Worktree, WorktreeManager};

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

#[derive(Serialize)]
pub struct SyncResult {
    pub synced: usize,
    pub closed: usize,
}

#[derive(Serialize)]
pub struct TicketDetail {
    pub agent_totals: Option<TicketAgentTotals>,
    pub worktrees: Vec<Worktree>,
}

pub async fn list_all_tickets(
    State(state): State<AppState>,
) -> Result<Json<Vec<Ticket>>, ApiError> {
    let db = state.db.lock().await;
    let syncer = TicketSyncer::new(&db);
    let tickets = syncer.list(None)?;
    Ok(Json(tickets))
}

pub async fn list_tickets(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<Vec<Ticket>>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    RepoManager::new(&db, &config).get_by_id(&repo_id)?;
    let syncer = TicketSyncer::new(&db);
    let tickets = syncer.list(Some(&repo_id))?;
    Ok(Json(tickets))
}

pub async fn sync_tickets(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<SyncResult>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let repo = RepoManager::new(&db, &config).get_by_id(&repo_id)?;
    let source_mgr = IssueSourceManager::new(&db);
    let syncer = TicketSyncer::new(&db);

    let sources = source_mgr.list(&repo.id)?;
    let mut total_synced = 0usize;
    let mut total_closed = 0usize;

    if sources.is_empty() {
        // Backward compat: auto-detect GitHub from remote URL
        if let Some((owner, name)) = github::parse_github_remote(&repo.remote_url) {
            let tickets = github::sync_github_issues(&owner, &name)?;
            let (synced, closed) = syncer.sync_and_close_tickets(&repo.id, "github", &tickets);
            total_synced += synced;
            total_closed += closed;
        }
    } else {
        for source in sources {
            match source.source_type.as_str() {
                "github" => {
                    if let Ok(cfg) = serde_json::from_str::<GitHubConfig>(&source.config_json) {
                        if let Ok(tickets) = github::sync_github_issues(&cfg.owner, &cfg.repo) {
                            let (synced, closed) =
                                syncer.sync_and_close_tickets(&repo.id, "github", &tickets);
                            total_synced += synced;
                            total_closed += closed;
                        }
                    }
                }
                "jira" => {
                    if let Ok(cfg) = serde_json::from_str::<JiraConfig>(&source.config_json) {
                        if let Ok(tickets) = jira_acli::sync_jira_issues_acli(&cfg.jql, &cfg.url) {
                            let (synced, closed) =
                                syncer.sync_and_close_tickets(&repo.id, "jira", &tickets);
                            total_synced += synced;
                            total_closed += closed;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    state.events.emit(ConductorEvent::TicketsSynced {
        repo_id: repo.id.clone(),
    });
    Ok(Json(SyncResult {
        synced: total_synced,
        closed: total_closed,
    }))
}

pub async fn ticket_detail(
    State(state): State<AppState>,
    Path(ticket_id): Path<String>,
) -> Result<Json<TicketDetail>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;

    let agent_mgr = AgentManager::new(&db);
    let all_totals = agent_mgr.totals_by_ticket_all()?;
    let agent_totals = all_totals.get(&ticket_id).cloned();

    let wt_mgr = WorktreeManager::new(&db, &config);
    let worktrees = wt_mgr.list_by_ticket(&ticket_id)?;

    Ok(Json(TicketDetail {
        agent_totals,
        worktrees,
    }))
}
