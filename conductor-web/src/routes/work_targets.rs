use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use conductor_core::config::{save_config, WorkTarget};

use crate::error::ApiError;
use crate::events::ConductorEvent;
use crate::state::AppState;

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
