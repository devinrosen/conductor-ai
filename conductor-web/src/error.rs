use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use conductor_core::error::ConductorError;

pub struct ApiError(pub ConductorError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            ConductorError::RepoNotFound { .. }
            | ConductorError::WorktreeNotFound { .. }
            | ConductorError::TicketNotFound { .. }
            | ConductorError::WorkflowRunNotFound { .. } => StatusCode::NOT_FOUND,
            ConductorError::RepoAlreadyExists { .. }
            | ConductorError::WorktreeAlreadyExists { .. }
            | ConductorError::IssueSourceAlreadyExists { .. }
            | ConductorError::TicketAlreadyLinked
            | ConductorError::WorkflowRunAlreadyActive { .. } => StatusCode::CONFLICT,
            ConductorError::TicketSync(_) => StatusCode::BAD_GATEWAY,
            ConductorError::Agent(_)
            | ConductorError::InvalidInput(_)
            | ConductorError::UnknownSourceType(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status.is_server_error() {
            tracing::error!(status = status.as_u16(), error = %self.0, "request failed");
        } else {
            tracing::warn!(status = status.as_u16(), error = %self.0, "request error");
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[test]
    fn unknown_source_type_maps_to_400() {
        let err = ApiError(ConductorError::UnknownSourceType("bogus".into()));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
