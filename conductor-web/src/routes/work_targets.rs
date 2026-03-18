use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::config::save_config;
use conductor_core::models;

use crate::error::ApiError;
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
