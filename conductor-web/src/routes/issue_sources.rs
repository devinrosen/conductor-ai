use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use conductor_core::github::parse_github_remote;
use conductor_core::issue_source::{IssueSource, IssueSourceManager};
use conductor_core::repo::RepoManager;

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateIssueSourceRequest {
    pub source_type: String,
    pub config_json: Option<String>,
}

pub async fn list_issue_sources(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<Vec<IssueSource>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = IssueSourceManager::new(&db);
    let sources = mgr.list(&repo_id)?;
    Ok(Json(sources))
}

pub async fn create_issue_source(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    Json(body): Json<CreateIssueSourceRequest>,
) -> Result<(StatusCode, Json<IssueSource>), ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let repo_mgr = RepoManager::new(&db, &config);

    // Look up the repo to get its slug and remote_url
    let repos = repo_mgr.list()?;
    let repo = repos.iter().find(|r| r.id == repo_id).ok_or_else(|| {
        conductor_core::error::ConductorError::RepoNotFound {
            slug: repo_id.clone(),
        }
    })?;

    let config_json = match body.source_type.as_str() {
        "github" => {
            if let Some(ref json) = body.config_json {
                json.clone()
            } else {
                // Auto-infer from remote URL
                let (owner, repo_name) = parse_github_remote(&repo.remote_url).ok_or_else(
                    || {
                        conductor_core::error::ConductorError::TicketSync(
                            "Cannot infer GitHub owner/repo from remote URL. Provide config_json manually.".to_string(),
                        )
                    },
                )?;
                serde_json::json!({"owner": owner, "repo": repo_name}).to_string()
            }
        }
        "jira" => body.config_json.clone().ok_or_else(|| {
            conductor_core::error::ConductorError::TicketSync(
                "Jira sources require config_json with jql and url fields".to_string(),
            )
        })?,
        _ => {
            return Err(ApiError(conductor_core::error::ConductorError::TicketSync(
                format!("Unknown source type: {}", body.source_type),
            )));
        }
    };

    // Validate JSON
    serde_json::from_str::<serde_json::Value>(&config_json).map_err(|e| {
        ApiError(conductor_core::error::ConductorError::TicketSync(format!(
            "Invalid config JSON: {e}"
        )))
    })?;

    let source_mgr = IssueSourceManager::new(&db);
    let source = source_mgr.add(&repo_id, &body.source_type, &config_json, &repo.slug)?;

    state.events.emit(ConductorEvent::IssueSourcesChanged {
        repo_id: repo_id.clone(),
    });

    Ok((StatusCode::CREATED, Json(source)))
}

pub async fn delete_issue_source(
    State(state): State<AppState>,
    Path((repo_id, source_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let db = state.db.lock().await;
    let mgr = IssueSourceManager::new(&db);
    mgr.remove(&source_id)?;

    state
        .events
        .emit(ConductorEvent::IssueSourcesChanged { repo_id });

    Ok(StatusCode::NO_CONTENT)
}
