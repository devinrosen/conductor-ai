use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::config::{save_config, WorkTarget};
use conductor_core::models;

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

#[derive(Serialize)]
pub struct GlobalModelResponse {
    pub model: Option<String>,
}

#[derive(Deserialize)]
pub struct SetGlobalModelRequest {
    pub model: Option<String>,
}

pub async fn get_global_model(
    State(state): State<AppState>,
) -> Result<Json<GlobalModelResponse>, ApiError> {
    let config = state.config.read().await;
    Ok(Json(GlobalModelResponse {
        model: config.general.model.clone(),
    }))
}

pub async fn patch_global_model(
    State(state): State<AppState>,
    Json(body): Json<SetGlobalModelRequest>,
) -> Result<Json<GlobalModelResponse>, ApiError> {
    let mut config = state.config.write().await;
    config.general.model = body.model.filter(|m| !m.trim().is_empty());
    save_config(&config)?;
    Ok(Json(GlobalModelResponse {
        model: config.general.model.clone(),
    }))
}

#[derive(Deserialize)]
pub struct CreateWorkTargetRequest {
    pub name: String,
    pub command: String,
    #[serde(rename = "type")]
    pub target_type: String,
}

pub async fn list_work_targets(
    State(state): State<AppState>,
) -> Result<Json<Vec<WorkTarget>>, ApiError> {
    let config = state.config.read().await;
    Ok(Json(config.general.work_targets.clone()))
}

pub async fn create_work_target(
    State(state): State<AppState>,
    Json(body): Json<CreateWorkTargetRequest>,
) -> Result<(StatusCode, Json<Vec<WorkTarget>>), ApiError> {
    let mut config = state.config.write().await;
    let target = WorkTarget {
        name: body.name,
        command: body.command,
        target_type: body.target_type,
    };
    config.general.work_targets.push(target);
    save_config(&config)?;
    let targets = config.general.work_targets.clone();
    state.events.emit(ConductorEvent::WorkTargetsChanged);
    Ok((StatusCode::CREATED, Json(targets)))
}

pub async fn delete_work_target(
    State(state): State<AppState>,
    Path(index): Path<usize>,
) -> Result<Json<Vec<WorkTarget>>, ApiError> {
    let mut config = state.config.write().await;
    if index >= config.general.work_targets.len() {
        return Err(ApiError(conductor_core::error::ConductorError::Config(
            format!(
                "work target index {} out of range (have {})",
                index,
                config.general.work_targets.len()
            ),
        )));
    }
    config.general.work_targets.remove(index);
    save_config(&config)?;
    let targets = config.general.work_targets.clone();
    state.events.emit(ConductorEvent::WorkTargetsChanged);
    Ok(Json(targets))
}

/// Replace the entire work targets list (used for reordering).
pub async fn replace_work_targets(
    State(state): State<AppState>,
    Json(targets): Json<Vec<CreateWorkTargetRequest>>,
) -> Result<Json<Vec<WorkTarget>>, ApiError> {
    let mut config = state.config.write().await;
    config.general.work_targets = targets
        .into_iter()
        .map(|t| WorkTarget {
            name: t.name,
            command: t.command,
            target_type: t.target_type,
        })
        .collect();
    save_config(&config)?;
    let result = config.general.work_targets.clone();
    state.events.emit(ConductorEvent::WorkTargetsChanged);
    Ok(Json(result))
}

/// Response type for the known models list.
#[derive(Serialize)]
pub struct KnownModelResponse {
    pub id: &'static str,
    pub alias: &'static str,
    pub tier: u8,
    pub tier_label: &'static str,
    pub description: &'static str,
}

/// Returns the curated list of known Claude models.
pub async fn list_known_models() -> Json<Vec<KnownModelResponse>> {
    let models: Vec<KnownModelResponse> = models::KNOWN_MODELS
        .iter()
        .map(|m| KnownModelResponse {
            id: m.id,
            alias: m.alias,
            tier: m.tier as u8,
            tier_label: m.tier_label(),
            description: m.description,
        })
        .collect();
    Json(models)
}

/// Request body for prompt-based model suggestion.
#[derive(Deserialize)]
pub struct SuggestModelRequest {
    pub prompt: String,
}

/// Response for model suggestion.
#[derive(Serialize)]
pub struct SuggestModelResponse {
    pub suggested: &'static str,
}

/// Suggest a model based on prompt text using keyword heuristics.
pub async fn suggest_model(Json(body): Json<SuggestModelRequest>) -> Json<SuggestModelResponse> {
    Json(SuggestModelResponse {
        suggested: models::suggest_model(&body.prompt),
    })
}
