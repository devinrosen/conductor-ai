use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::github::{
    discover_github_repos, list_github_orgs, list_open_prs, DiscoveredRepo, GithubPr,
};
use conductor_core::repo::{derive_local_path, derive_slug_from_url, Repo, RepoManager};

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RegisterRepoRequest {
    pub remote_url: String,
    pub slug: Option<String>,
    pub local_path: Option<String>,
    pub workspace_dir: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/repos",
    responses(
        (status = 200, description = "List of registered repos", body = Vec<Repo>),
    ),
    tag = "repos",
)]
pub async fn list_repos(State(state): State<AppState>) -> Result<Json<Vec<Repo>>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = RepoManager::new(&db, &config);
    let repos = mgr.list()?;
    Ok(Json(repos))
}

#[utoipa::path(
    post,
    path = "/api/repos",
    request_body(content = RegisterRepoRequest, description = "Repo registration details"),
    responses(
        (status = 201, description = "Repo registered", body = Repo),
    ),
    tag = "repos",
)]
pub async fn register_repo(
    State(state): State<AppState>,
    Json(body): Json<RegisterRepoRequest>,
) -> Result<(StatusCode, Json<Repo>), ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = RepoManager::new(&db, &config);
    let slug = body
        .slug
        .unwrap_or_else(|| derive_slug_from_url(&body.remote_url));
    let local_path = body
        .local_path
        .unwrap_or_else(|| derive_local_path(&config, &slug));
    let repo = mgr.register(
        &slug,
        &local_path,
        &body.remote_url,
        body.workspace_dir.as_deref(),
    )?;
    state.events.emit(ConductorEvent::RepoRegistered {
        id: repo.id.clone(),
    });
    Ok((StatusCode::CREATED, Json(repo)))
}

#[utoipa::path(
    delete,
    path = "/api/repos/{id}",
    params(
        ("id" = String, Path, description = "Repo ID"),
    ),
    responses(
        (status = 204, description = "Repo unregistered"),
        (status = 404, description = "Repo not found"),
    ),
    tag = "repos",
)]
pub async fn unregister_repo(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = RepoManager::new(&db, &config);
    mgr.unregister_by_id(&id)?;
    state.events.emit(ConductorEvent::RepoUnregistered { id });
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SetModelRequest {
    pub model: Option<String>,
}

#[utoipa::path(
    patch,
    path = "/api/repos/{id}/model",
    params(
        ("id" = String, Path, description = "Repo ID"),
    ),
    request_body(content = SetModelRequest, description = "Model to set for repo"),
    responses(
        (status = 200, description = "Updated repo", body = Repo),
        (status = 404, description = "Repo not found"),
    ),
    tag = "repos",
)]
pub async fn patch_repo_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SetModelRequest>,
) -> Result<Json<Repo>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = RepoManager::new(&db, &config);
    let repo = mgr.get_by_id(&id)?;
    mgr.set_model(&repo.slug, body.model.as_deref())?;
    // Re-read to get updated computed fields
    let updated = mgr.get_by_id(&id)?;
    Ok(Json(updated))
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateRepoSettingsRequest {
    pub allow_agent_issue_creation: Option<bool>,
}

/// Update per-repo settings (e.g. agent issue creation opt-in).
#[utoipa::path(
    patch,
    path = "/api/repos/{id}/settings",
    params(
        ("id" = String, Path, description = "Repo ID"),
    ),
    request_body(content = UpdateRepoSettingsRequest, description = "Repo settings to update"),
    responses(
        (status = 200, description = "Updated repo", body = Repo),
        (status = 404, description = "Repo not found"),
    ),
    tag = "repos",
)]
pub async fn update_repo_settings(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateRepoSettingsRequest>,
) -> Result<Json<Repo>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = RepoManager::new(&db, &config);
    if let Some(allow) = body.allow_agent_issue_creation {
        mgr.set_allow_agent_issue_creation(&id, allow)?;
    }
    let repo = mgr.get_by_id(&id)?;
    Ok(Json(repo))
}

/// A repo discovered via GitHub with a flag indicating if it's already registered.
#[derive(Serialize, utoipa::ToSchema)]
pub struct DiscoverableRepo {
    #[serde(flatten)]
    pub repo: DiscoveredRepo,
    pub already_registered: bool,
    pub registered_id: Option<String>,
}

/// GET /api/github/orgs — list GitHub organizations the authenticated user belongs to.
#[utoipa::path(
    get,
    path = "/api/github/orgs",
    responses(
        (status = 200, description = "List of GitHub organizations", body = Vec<String>),
    ),
    tag = "repos",
)]
pub async fn list_github_orgs_handler() -> Result<Json<Vec<String>>, ApiError> {
    let orgs = list_github_orgs()?;
    Ok(Json(orgs))
}

#[derive(Deserialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct DiscoverReposQuery {
    pub owner: Option<String>,
}

/// GET /api/github/repos?owner=<org> — fetch repos for the given org (or personal if omitted)
/// and annotate each with whether it's already registered in Conductor.
#[utoipa::path(
    get,
    path = "/api/github/repos",
    params(DiscoverReposQuery),
    responses(
        (status = 200, description = "List of discoverable GitHub repos", body = Vec<DiscoverableRepo>),
    ),
    tag = "repos",
)]
pub async fn discover_github_repos_handler(
    State(state): State<AppState>,
    Query(params): Query<DiscoverReposQuery>,
) -> Result<Json<Vec<DiscoverableRepo>>, ApiError> {
    let discovered = discover_github_repos(params.owner.as_deref())?;

    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = RepoManager::new(&db, &config);
    let registered = mgr.list()?;

    let result = discovered
        .into_iter()
        .map(|repo| {
            let registered_entry = registered
                .iter()
                .find(|r| r.remote_url == repo.clone_url || r.remote_url == repo.ssh_url);
            DiscoverableRepo {
                already_registered: registered_entry.is_some(),
                registered_id: registered_entry.map(|r| r.id.clone()),
                repo,
            }
        })
        .collect();

    Ok(Json(result))
}

/// GET /api/repos/{id}/prs — list open PRs for the given repo.
#[utoipa::path(
    get,
    path = "/api/repos/{id}/prs",
    params(
        ("id" = String, Path, description = "Repo ID"),
    ),
    responses(
        (status = 200, description = "List of open PRs", body = Vec<GithubPr>),
        (status = 404, description = "Repo not found"),
    ),
    tag = "repos",
)]
pub async fn list_prs(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<GithubPr>>, ApiError> {
    let remote_url = {
        let db = state.db.lock().await;
        let config = state.config.read().await;
        let mgr = RepoManager::new(&db, &config);
        mgr.get_by_id(&id)?.remote_url
    };
    let prs = match tokio::task::spawn_blocking(move || list_open_prs(&remote_url)).await {
        Ok(Ok(prs)) => prs,
        Ok(Err(e)) => {
            tracing::warn!("list_open_prs failed: {e}");
            vec![]
        }
        Err(e) => {
            tracing::warn!("list_open_prs task panicked: {e}");
            vec![]
        }
    };
    let prs = tokio::task::spawn_blocking(move || list_open_prs(&remote_url).unwrap_or_default())
        .await
        .unwrap_or_default();
    Ok(Json(prs))
}
