use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use conductor_core::error::ConductorError;

pub struct ApiError(pub ConductorError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            ConductorError::RepoNotFound { .. }
            | ConductorError::WorktreeNotFound { .. }
            | ConductorError::TicketNotFound { .. } => StatusCode::NOT_FOUND,
            ConductorError::RepoAlreadyExists { .. }
            | ConductorError::WorktreeAlreadyExists { .. }
            | ConductorError::IssueSourceAlreadyExists { .. } => StatusCode::CONFLICT,
            ConductorError::TicketSync(_) => StatusCode::BAD_GATEWAY,
            ConductorError::Agent(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = serde_json::json!({ "error": self.0.to_string() });
        (status, axum::Json(body)).into_response()
    }
}

impl From<ConductorError> for ApiError {
    fn from(err: ConductorError) -> Self {
        ApiError(err)
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(err: rusqlite::Error) -> Self {
        ApiError(ConductorError::Database(err))
    }
}
