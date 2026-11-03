use axum::Json;
use serde::Serialize;

#[derive(Serialize, utoipa::ToSchema)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub built_at: &'static str,
}

#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthResponse),
    ),
    tag = "health",
)]
pub async fn health() -> Json<HealthResponse> {
    let info = conductor_core::build_info::get();
    Json(HealthResponse {
        status: "ok",
        version: info.git_sha,
        built_at: info.commit_time,
    })
}
