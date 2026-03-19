use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::config::RepoConfig;
use conductor_core::github::{discover_github_repos, list_github_orgs, DiscoveredRepo};
use conductor_core::repo::{derive_local_path, derive_slug_from_url, Repo, RepoManager};

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

/// API response that includes resolved per-repo config values alongside core Repo data.
/// The frontend sees `default_branch` and `model` as before, but they're now resolved
/// from `.conductor/config.toml` with global config fallback.
#[derive(Serialize)]
pub struct RepoResponse {
    #[serde(flatten)]
    pub repo: Repo,
    pub default_branch: String,
    pub model: Option<String>,
}

fn repo_to_response(repo: Repo, global_default_branch: &str) -> RepoResponse {
    let repo_config =
        RepoConfig::load(std::path::Path::new(&repo.local_path)).unwrap_or_else(|e| {
            tracing::warn!(
                "Failed to load .conductor/config.toml for repo '{}': {e}; using defaults",
                repo.slug,
            );
            RepoConfig::default()
        });
    let default_branch = repo_config
        .defaults
        .default_branch
        .unwrap_or_else(|| global_default_branch.to_string());
    let model = repo_config.defaults.model;
    RepoResponse {
        repo,
        default_branch,
        model,
    }
}

#[derive(Deserialize)]
pub struct RegisterRepoRequest {
    pub remote_url: String,
    pub slug: Option<String>,
    pub local_path: Option<String>,
    pub workspace_dir: Option<String>,
}

pub async fn list_repos(
    State(state): State<AppState>,
) -> Result<Json<Vec<RepoResponse>>, ApiError> {
    let (repos, default_branch) = {
        let db = state.db.lock().await;
        let config = state.config.read().await;
        let mgr = RepoManager::new(&db, &config);
        (mgr.list()?, config.defaults.default_branch.clone())
    };
    // RepoConfig::load performs file I/O — run off the tokio worker thread.
    let responses = tokio::task::spawn_blocking(move || {
        repos
            .into_iter()
            .map(|r| repo_to_response(r, &default_branch))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| {
        ApiError(conductor_core::error::ConductorError::Config(format!(
            "spawn_blocking join failed: {e}"
        )))
    })?;
    Ok(Json(responses))
}

pub async fn register_repo(
    State(state): State<AppState>,
    Json(body): Json<RegisterRepoRequest>,
) -> Result<(StatusCode, Json<RepoResponse>), ApiError> {
    let (repo, default_branch) = {
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
        (repo, config.defaults.default_branch.clone())
    };
    // repo_to_response performs file I/O — run off the tokio worker thread.
    let response = tokio::task::spawn_blocking(move || repo_to_response(repo, &default_branch))
        .await
        .map_err(|e| {
            ApiError(conductor_core::error::ConductorError::Config(format!(
                "spawn_blocking join failed: {e}"
            )))
        })?;
    Ok((StatusCode::CREATED, Json(response)))
}

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

#[derive(Deserialize)]
pub struct SetModelRequest {
    pub model: Option<String>,
}

pub async fn patch_repo_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SetModelRequest>,
) -> Result<Json<RepoResponse>, ApiError> {
    let (repo, default_branch) = {
        let db = state.db.lock().await;
        let config = state.config.read().await;
        let mgr = RepoManager::new(&db, &config);
        let repo = mgr.get_by_id(&id)?;
        mgr.set_model(&repo, body.model)?;
        (repo, config.defaults.default_branch.clone())
    };
    // repo_to_response performs file I/O — run off the tokio worker thread.
    let response = tokio::task::spawn_blocking(move || repo_to_response(repo, &default_branch))
        .await
        .map_err(|e| {
            ApiError(conductor_core::error::ConductorError::Config(format!(
                "spawn_blocking join failed: {e}"
            )))
        })?;
    Ok(Json(response))
}

#[derive(Deserialize)]
pub struct UpdateRepoSettingsRequest {
    pub allow_agent_issue_creation: Option<bool>,
}

/// Update per-repo settings (e.g. agent issue creation opt-in).
pub async fn update_repo_settings(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateRepoSettingsRequest>,
) -> Result<Json<RepoResponse>, ApiError> {
    let (repo, default_branch) = {
        let db = state.db.lock().await;
        let config = state.config.read().await;
        let mgr = RepoManager::new(&db, &config);
        if let Some(allow) = body.allow_agent_issue_creation {
            mgr.set_allow_agent_issue_creation(&id, allow)?;
        }
        let repo = mgr.get_by_id(&id)?;
        (repo, config.defaults.default_branch.clone())
    };
    // repo_to_response performs file I/O — run off the tokio worker thread.
    let response = tokio::task::spawn_blocking(move || repo_to_response(repo, &default_branch))
        .await
        .map_err(|e| {
            ApiError(conductor_core::error::ConductorError::Config(format!(
                "spawn_blocking join failed: {e}"
            )))
        })?;
    Ok(Json(response))
}

/// A repo discovered via GitHub with a flag indicating if it's already registered.
#[derive(Serialize)]
pub struct DiscoverableRepo {
    #[serde(flatten)]
    pub repo: DiscoveredRepo,
    pub already_registered: bool,
    pub registered_id: Option<String>,
}

/// GET /api/github/orgs — list GitHub organizations the authenticated user belongs to.
pub async fn list_github_orgs_handler() -> Result<Json<Vec<String>>, ApiError> {
    let orgs = list_github_orgs()?;
    Ok(Json(orgs))
}

#[derive(Deserialize)]
pub struct DiscoverReposQuery {
    pub owner: Option<String>,
}

/// GET /api/github/repos?owner=<org> — fetch repos for the given org (or personal if omitted)
/// and annotate each with whether it's already registered in Conductor.
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
