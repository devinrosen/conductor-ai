use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use conductor_core::config::save_config;
use conductor_core::models;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize, utoipa::ToSchema)]
pub struct GlobalModelResponse {
    pub model: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SetGlobalModelRequest {
    pub model: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/config/model",
    responses(
        (status = 200, description = "Current global model configuration", body = GlobalModelResponse),
    ),
    tag = "model_config",
)]
pub async fn get_global_model(
    State(state): State<AppState>,
) -> Result<Json<GlobalModelResponse>, ApiError> {
    let config = state.config.read().await;
    Ok(Json(GlobalModelResponse {
        model: config.general.model.clone(),
    }))
}

#[utoipa::path(
    patch,
    path = "/api/config/model",
    request_body(content = SetGlobalModelRequest, description = "New global model setting"),
    responses(
        (status = 200, description = "Updated global model configuration", body = GlobalModelResponse),
    ),
    tag = "model_config",
)]
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
#[derive(Serialize, utoipa::ToSchema)]
pub struct KnownModelResponse {
    pub id: &'static str,
    pub alias: &'static str,
    pub tier: u8,
    pub tier_label: &'static str,
    pub description: &'static str,
}

/// Returns the curated list of known Claude models.
#[utoipa::path(
    get,
    path = "/api/config/known-models",
    responses(
        (status = 200, description = "List of known Claude models", body = Vec<KnownModelResponse>),
    ),
    tag = "model_config",
)]
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
#[derive(Deserialize, utoipa::ToSchema)]
pub struct SuggestModelRequest {
    pub prompt: String,
}

/// Response for model suggestion.
#[derive(Serialize, utoipa::ToSchema)]
pub struct SuggestModelResponse {
    pub suggested: &'static str,
}

/// Suggest a model based on prompt text using keyword heuristics.
#[utoipa::path(
    post,
    path = "/api/config/suggest-model",
    request_body(content = SuggestModelRequest, description = "Prompt text to base suggestion on"),
    responses(
        (status = 200, description = "Suggested model", body = SuggestModelResponse),
    ),
    tag = "model_config",
)]
pub async fn suggest_model(Json(body): Json<SuggestModelRequest>) -> Json<SuggestModelResponse> {
    Json(SuggestModelResponse {
        suggested: models::suggest_model(&body.prompt),
    })
}
