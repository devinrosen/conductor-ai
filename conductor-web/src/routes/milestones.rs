use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;

use conductor_core::milestone::{
    Deliverable, Milestone, MilestoneManager, MilestoneProgress, MilestoneStatus,
};
use conductor_core::repo::RepoManager;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateMilestoneRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub target_date: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateStatusRequest {
    pub status: String,
}

#[derive(Deserialize)]
pub struct CreateDeliverableRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub feature_id: Option<String>,
}

pub async fn list_milestones(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> Result<Json<Vec<Milestone>>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    RepoManager::new(&db, &config).get_by_id(&repo_id)?;
    let mgr = MilestoneManager::new(&db, &config);
    let milestones = mgr.list_milestones(&repo_id)?;
    Ok(Json(milestones))
}

pub async fn create_milestone(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    Json(body): Json<CreateMilestoneRequest>,
) -> Result<Json<Milestone>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    RepoManager::new(&db, &config).get_by_id(&repo_id)?;
    let mgr = MilestoneManager::new(&db, &config);
    let ms = mgr.create_milestone(
        &repo_id,
        &body.name,
        &body.description,
        body.target_date.as_deref(),
    )?;
    Ok(Json(ms))
}

pub async fn get_milestone_progress(
    State(state): State<AppState>,
    Path(milestone_id): Path<String>,
) -> Result<Json<MilestoneProgress>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = MilestoneManager::new(&db, &config);
    let progress = mgr.milestone_progress(&milestone_id)?;
    Ok(Json(progress))
}

pub async fn update_milestone_status(
    State(state): State<AppState>,
    Path(milestone_id): Path<String>,
    Json(body): Json<UpdateStatusRequest>,
) -> Result<Json<Milestone>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = MilestoneManager::new(&db, &config);
    let status: MilestoneStatus = body
        .status
        .parse()
        .map_err(|e: String| conductor_core::error::ConductorError::InvalidInput(e))?;
    mgr.update_milestone_status(&milestone_id, status)?;
    let ms = mgr.get_milestone_by_id(&milestone_id)?;
    Ok(Json(ms))
}

pub async fn delete_milestone(
    State(state): State<AppState>,
    Path(milestone_id): Path<String>,
) -> Result<Json<()>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = MilestoneManager::new(&db, &config);
    mgr.delete_milestone(&milestone_id)?;
    Ok(Json(()))
}

pub async fn list_deliverables(
    State(state): State<AppState>,
    Path(milestone_id): Path<String>,
) -> Result<Json<Vec<Deliverable>>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = MilestoneManager::new(&db, &config);
    let deliverables = mgr.list_deliverables(&milestone_id)?;
    Ok(Json(deliverables))
}

pub async fn create_deliverable(
    State(state): State<AppState>,
    Path(milestone_id): Path<String>,
    Json(body): Json<CreateDeliverableRequest>,
) -> Result<Json<Deliverable>, ApiError> {
    let db = state.db.lock().await;
    let config = state.config.read().await;
    let mgr = MilestoneManager::new(&db, &config);
    let d = mgr.create_deliverable(
        &milestone_id,
        &body.name,
        &body.description,
        body.feature_id.as_deref(),
    )?;
    Ok(Json(d))
}
