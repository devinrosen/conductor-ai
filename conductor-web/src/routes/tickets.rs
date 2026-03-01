use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;

use conductor_core::github;
use conductor_core::issue_source::{GitHubConfig, IssueSourceManager, JiraConfig};
use conductor_core::jira_acli;
use conductor_core::repo::RepoManager;
use conductor_core::tickets::{Ticket, TicketSyncer};

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

#[derive(Serialize)]
pub struct SyncResult {
    pub synced: usize,
    pub closed: usize,
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
            let synced_ids: Vec<&str> = tickets.iter().map(|t| t.source_id.as_str()).collect();
            total_synced += syncer.upsert_tickets(&repo.id, &tickets)?;
            total_closed += syncer
                .close_missing_tickets(&repo.id, "github", &synced_ids)
                .unwrap_or(0);
            let _ = syncer.mark_worktrees_for_closed_tickets(&repo.id);
        }
    } else {
        for source in sources {
            match source.source_type.as_str() {
                "github" => {
                    if let Ok(cfg) = serde_json::from_str::<GitHubConfig>(&source.config_json) {
                        if let Ok(tickets) = github::sync_github_issues(&cfg.owner, &cfg.repo) {
                            let synced_ids: Vec<&str> =
                                tickets.iter().map(|t| t.source_id.as_str()).collect();
                            total_synced += syncer.upsert_tickets(&repo.id, &tickets).unwrap_or(0);
                            total_closed += syncer
                                .close_missing_tickets(&repo.id, "github", &synced_ids)
                                .unwrap_or(0);
                            let _ = syncer.mark_worktrees_for_closed_tickets(&repo.id);
                        }
                    }
                }
                "jira" => {
                    if let Ok(cfg) = serde_json::from_str::<JiraConfig>(&source.config_json) {
                        if let Ok(tickets) = jira_acli::sync_jira_issues_acli(&cfg.jql, &cfg.url) {
                            let synced_ids: Vec<&str> =
                                tickets.iter().map(|t| t.source_id.as_str()).collect();
                            total_synced += syncer.upsert_tickets(&repo.id, &tickets).unwrap_or(0);
                            total_closed += syncer
                                .close_missing_tickets(&repo.id, "jira", &synced_ids)
                                .unwrap_or(0);
                            let _ = syncer.mark_worktrees_for_closed_tickets(&repo.id);
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
