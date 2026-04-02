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

impl From<tokio::task::JoinError> for ApiError {
    fn from(err: tokio::task::JoinError) -> Self {
        if err.is_panic() {
            ApiError(ConductorError::Internal("internal server error".into()))
        } else {
            ApiError(ConductorError::Internal(format!(
                "blocking task failed: {err}"
            )))
        }
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

    #[tokio::test]
    async fn join_error_panic_sanitized_to_generic_message() {
        // Verify that a panicking spawn_blocking task does NOT leak the panic
        // payload (e.g. internal file paths) into the ApiError message.
        let result = tokio::task::spawn_blocking(|| -> () {
            panic!("secret internal path: /home/user/.conductor/conductor.db");
        })
        .await;
        let join_err = result.unwrap_err();
        assert!(join_err.is_panic(), "expected a panic JoinError");
        let api_err = ApiError::from(join_err);
        match api_err.0 {
            ConductorError::Internal(msg) => {
                assert_eq!(msg, "internal server error");
            }
            other => panic!("expected ConductorError::Internal, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn join_error_cancellation_includes_context() {
        // Verify the non-panic path includes diagnostic context.
        let handle = tokio::task::spawn(async {});
        handle.abort();
        let join_err = handle.await.unwrap_err();
        assert!(!join_err.is_panic(), "expected a cancellation JoinError");
        let api_err = ApiError::from(join_err);
        match api_err.0 {
            ConductorError::Internal(msg) => {
                assert!(
                    msg.starts_with("blocking task failed:"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("expected ConductorError::Internal, got {:?}", other),
        }
    }
}
