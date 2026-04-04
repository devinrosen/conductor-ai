use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use conductor_core::error::ConductorError;

pub enum ApiError {
    Core(ConductorError),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Internal(msg) => {
                tracing::error!(error = %msg, "internal request error");
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
            ApiError::Core(ref err) => {
                let status = match err {
                    ConductorError::RepoNotFound { .. }
                    | ConductorError::WorktreeNotFound { .. }
                    | ConductorError::TicketNotFound { .. }
                    | ConductorError::WorkflowRunNotFound { .. }
                    | ConductorError::AgentRunNotFound { .. }
                    | ConductorError::FeedbackNotFound { .. }
                    | ConductorError::AgentRunNotInConversation { .. }
                    | ConductorError::FeedbackRunMismatch { .. } => StatusCode::NOT_FOUND,
                    ConductorError::RepoAlreadyExists { .. }
                    | ConductorError::WorktreeAlreadyExists { .. }
                    | ConductorError::IssueSourceAlreadyExists { .. }
                    | ConductorError::TicketAlreadyLinked
                    | ConductorError::WorkflowRunAlreadyActive { .. } => StatusCode::CONFLICT,
                    ConductorError::TicketSync(_) => StatusCode::BAD_GATEWAY,
                    ConductorError::NoPendingFeedbackForRun { .. }
                    | ConductorError::Agent(_)
                    | ConductorError::InvalidInput(_)
                    | ConductorError::UnknownSourceType(_) => StatusCode::BAD_REQUEST,
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                let msg = err.to_string();
                if status.is_server_error() {
                    tracing::error!(status = status.as_u16(), error = %err, "request failed");
                } else {
                    tracing::warn!(status = status.as_u16(), error = %err, "request error");
                }
                (status, msg)
            }
        };
        let body = serde_json::json!({ "error": message });
        (status, axum::Json(body)).into_response()
    }
}

impl From<ConductorError> for ApiError {
    fn from(err: ConductorError) -> Self {
        ApiError::Core(err)
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(err: rusqlite::Error) -> Self {
        ApiError::Core(ConductorError::Database(err))
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(err: tokio::task::JoinError) -> Self {
        if err.is_panic() {
            ApiError::Internal("internal server error".into())
        } else {
            ApiError::Internal(format!("blocking task failed: {err}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[test]
    fn internal_variant_maps_to_500() {
        let err = ApiError::Internal("something went wrong".into());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn unknown_source_type_maps_to_400() {
        let err = ApiError::Core(ConductorError::UnknownSourceType("bogus".into()));
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
        match api_err {
            ApiError::Internal(msg) => {
                assert_eq!(msg, "internal server error");
            }
            other => panic!(
                "expected ApiError::Internal, got {:?}",
                other.into_response().status()
            ),
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
        match api_err {
            ApiError::Internal(msg) => {
                assert!(
                    msg.starts_with("blocking task failed:"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!(
                "expected ApiError::Internal, got {:?}",
                other.into_response().status()
            ),
        }
    }
}
