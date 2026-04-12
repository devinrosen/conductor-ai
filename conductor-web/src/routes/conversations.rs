use axum::extract::{FromRequest, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use conductor_core::agent::AgentManager;
use conductor_core::agent::AgentRun;
use conductor_core::config::AgentPermissionMode;
use conductor_core::conversation::{
    Conversation, ConversationManager, ConversationScope, ConversationWithRuns,
};
use conductor_core::error::ConductorError;
use conductor_core::repo::RepoManager;
use conductor_core::worktree::WorktreeManager;

use crate::attachments::{parse_multipart_body, write_attachments_and_augment_prompt};
use crate::error::ApiError;
use crate::state::AppState;

use super::agents::spawn_headless_agent;

// ── Request / Response types ──────────────────────────────────────────────────

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateConversationRequest {
    pub scope: ConversationScope,
    pub scope_id: String,
}

#[derive(Deserialize, utoipa::IntoParams, utoipa::ToSchema)]
pub struct ListConversationsQuery {
    pub scope: ConversationScope,
    pub scope_id: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SendMessageRequest {
    pub prompt: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RespondToFeedbackRequest {
    pub response: String,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct RespondToFeedbackByIdRequest {
    pub run_id: String,
    pub feedback_id: String,
    pub response: String,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// POST /api/conversations — create a new conversation.
#[utoipa::path(
    post,
    path = "/api/conversations",
    request_body(content = CreateConversationRequest, description = "Conversation creation details"),
    responses(
        (status = 201, description = "Conversation created", body = Conversation),
    ),
    tag = "conversations",
)]
pub async fn create_conversation(
    State(state): State<AppState>,
    Json(body): Json<CreateConversationRequest>,
) -> Result<(StatusCode, Json<Conversation>), ApiError> {
    let db = state.db.lock().await;
    let mgr = ConversationManager::new(&db);
    let conversation = mgr.create(body.scope, &body.scope_id)?;
    Ok((StatusCode::CREATED, Json(conversation)))
}

/// GET /api/conversations?scope=&scope_id= — list conversations for a scope.
#[utoipa::path(
    get,
    path = "/api/conversations",
    params(ListConversationsQuery),
    responses(
        (status = 200, description = "List of conversations", body = Vec<Conversation>),
    ),
    tag = "conversations",
)]
pub async fn list_conversations(
    State(state): State<AppState>,
    Query(params): Query<ListConversationsQuery>,
) -> Result<Json<Vec<Conversation>>, ApiError> {
    let db = state.db.lock().await;
    let mgr = ConversationManager::new(&db);
    let conversations = mgr.list(&params.scope, &params.scope_id)?;
    Ok(Json(conversations))
}

/// GET /api/conversations/{id} — get conversation detail with associated runs.
#[utoipa::path(
    get,
    path = "/api/conversations/{id}",
    params(
        ("id" = String, Path, description = "Conversation ID"),
    ),
    responses(
        (status = 200, description = "Conversation with runs", body = ConversationWithRuns),
        (status = 404, description = "Conversation not found"),
    ),
    tag = "conversations",
)]
pub async fn get_conversation(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
) -> Result<Json<ConversationWithRuns>, ApiError> {
    let db = state.db.lock().await;
    let mgr = ConversationManager::new(&db);
    let conversation = mgr.get_with_runs(&conversation_id)?.ok_or_else(|| {
        ConductorError::ConversationNotFound {
            id: conversation_id.clone(),
        }
    })?;
    Ok(Json(conversation))
}

/// DELETE /api/conversations/{id} — hard-delete a conversation and its agent runs.
///
/// Returns 204 No Content on success. Returns 409 Conflict if the conversation
/// has an active or waiting agent run.
#[utoipa::path(
    delete,
    path = "/api/conversations/{id}",
    params(
        ("id" = String, Path, description = "Conversation ID"),
    ),
    responses(
        (status = 204, description = "Conversation deleted"),
        (status = 404, description = "Conversation not found"),
        (status = 409, description = "Conversation has an active agent run"),
    ),
    tag = "conversations",
)]
pub async fn delete_conversation(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let db = state.db.lock().await;
    ConversationManager::new(&db).delete(&conversation_id)?;
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/conversations/{id}/messages — send a message to a conversation.
///
/// Accepts both `application/json` and `multipart/form-data`. For multipart,
/// include a `prompt` text field and zero or more `attachment_N` file fields.
/// Supported attachment MIME types: image/png, image/jpeg, image/heic,
/// application/pdf, text/plain. Attachment files are saved to
/// `{worktree_path}/.conductor-attachments-{run_id}/` and their absolute paths
/// are appended to the prompt so the agent can read them.
///
/// Creates a new agent run, with automatic session resumption from the last
/// completed run. Returns the full `AgentRun` object immediately; the agent
/// runs asynchronously.
#[utoipa::path(
    post,
    path = "/api/conversations/{id}/messages",
    params(
        ("id" = String, Path, description = "Conversation ID"),
    ),
    request_body(
        content = SendMessageRequest,
        description = "Message prompt. Also accepts multipart/form-data with a 'prompt' text field and optional 'attachment_N' file fields (image/png, image/jpeg, image/heic, application/pdf, text/plain).",
    ),
    responses(
        (status = 201, description = "Agent run created", body = AgentRun),
        (status = 404, description = "Conversation not found"),
        (status = 415, description = "Unsupported Content-Type (expected application/json or multipart/form-data)"),
        (status = 422, description = "Missing or invalid field in request body"),
    ),
    tag = "conversations",
)]
pub async fn send_message(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    request: axum::extract::Request,
) -> Result<(StatusCode, Json<AgentRun>), ApiError> {
    // Read content-type before consuming the body.
    let content_type = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Pre-Phase: parse body before acquiring the DB lock.
    let (prompt, raw_attachments) = if content_type.starts_with("multipart/form-data") {
        parse_multipart_body(request, &state).await?
    } else if content_type.starts_with("application/json") || content_type.is_empty() {
        let Json(body) = Json::<SendMessageRequest>::from_request(request, &state)
            .await
            .map_err(|e| ApiError::UnprocessableEntity(e.to_string()))?;
        (body.prompt, vec![])
    } else {
        return Err(ApiError::UnsupportedMediaType(format!(
            "expected application/json or multipart/form-data, got: {content_type}"
        )));
    };

    // Phase 1: all DB work under the async lock.
    let (run, resume_session_id, working_dir, permission_mode, model) = {
        let db = state.db.lock().await;
        let config = state.config.read().await;

        let conv_mgr = ConversationManager::new(&db);

        // Fetch conversation to determine scope and path.
        let conv = conv_mgr.get(&conversation_id)?.ok_or_else(|| {
            ConductorError::Agent(format!("conversation {conversation_id} not found"))
        })?;

        // Resolve working directory, model, and permission mode based on scope.
        let (working_dir, model, permission_mode) = match &conv.scope {
            ConversationScope::Repo => {
                let repo = RepoManager::new(&db, &config).get_by_id(&conv.scope_id)?;
                let model = repo
                    .model
                    .as_deref()
                    .or(config.general.model.as_deref())
                    .map(str::to_string);
                (
                    repo.local_path.clone(),
                    model,
                    Some(AgentPermissionMode::RepoSafe),
                )
            }
            ConversationScope::Worktree => {
                let wt_mgr = WorktreeManager::new(&db, &config);
                let wt = wt_mgr.get_by_id(&conv.scope_id)?;
                let repo = RepoManager::new(&db, &config).get_by_id(&wt.repo_id)?;
                let model = wt
                    .model
                    .as_deref()
                    .or(repo.model.as_deref())
                    .or(config.general.model.as_deref())
                    .map(str::to_string);
                (wt.path.clone(), model, None)
            }
        };

        // Delegate run creation, concurrency guard, session lookup, and
        // metadata updates to ConversationManager::send_message.
        // The original prompt (without attachment paths) is stored in the DB.
        let (run, resume_session_id) =
            conv_mgr.send_message(&conversation_id, &prompt, None, model.as_deref())?;

        (run, resume_session_id, working_dir, permission_mode, model)
    };
    // DB and config locks are now dropped.

    // Phase 2: write attachment files to disk, build augmented prompt, spawn headless.
    let final_prompt =
        write_attachments_and_augment_prompt(&run.id, &working_dir, &prompt, &raw_attachments)?;

    spawn_headless_agent(
        &state,
        &run.id,
        &working_dir,
        &final_prompt,
        resume_session_id.as_deref(),
        model.as_deref(),
        None,
        permission_mode.as_ref(),
        None,
    )
    .await?;

    Ok((StatusCode::CREATED, Json(run)))
}

/// POST /api/conversations/{id}/feedback — submit feedback for a run using an
/// explicit feedback_id. This is the mobile-client entrypoint; the run_id is
/// used only to verify the run belongs to the conversation.
#[utoipa::path(
    post,
    path = "/api/conversations/{id}/feedback",
    params(
        ("id" = String, Path, description = "Conversation ID"),
    ),
    request_body(content = RespondToFeedbackByIdRequest, description = "Feedback response"),
    responses(
        (status = 200, description = "Updated agent run", body = AgentRun),
        (status = 404, description = "Conversation or run not found"),
    ),
    tag = "conversations",
)]
pub async fn respond_to_feedback(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    Json(body): Json<RespondToFeedbackByIdRequest>,
) -> Result<Json<AgentRun>, ApiError> {
    let db = state.db.lock().await;
    let agent_mgr = AgentManager::new(&db);

    let updated_run = agent_mgr.submit_feedback_for_conversation(
        &conversation_id,
        &body.run_id,
        &body.feedback_id,
        &body.response,
    )?;

    Ok(Json(updated_run))
}

/// POST /api/conversations/{id}/messages/{run_id}/respond — respond to a
/// human-in-the-loop feedback request for a specific run.
#[utoipa::path(
    post,
    path = "/api/conversations/{id}/messages/{run_id}/respond",
    params(
        ("id" = String, Path, description = "Conversation ID"),
        ("run_id" = String, Path, description = "Agent run ID"),
    ),
    request_body(content = RespondToFeedbackRequest, description = "Feedback response"),
    responses(
        (status = 200, description = "Updated agent run", body = AgentRun),
        (status = 404, description = "Conversation or run not found"),
    ),
    tag = "conversations",
)]
pub async fn respond_to_run_feedback(
    State(state): State<AppState>,
    Path((conversation_id, run_id)): Path<(String, String)>,
    Json(body): Json<RespondToFeedbackRequest>,
) -> Result<Json<AgentRun>, ApiError> {
    let db = state.db.lock().await;
    let agent_mgr = AgentManager::new(&db);

    let updated_run = agent_mgr.submit_pending_run_feedback_for_conversation(
        &conversation_id,
        &run_id,
        &body.response,
    )?;

    Ok(Json(updated_run))
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::routes::api_router;
    use crate::test_helpers::seeded_state;

    async fn send_post_json(uri: &str, body: serde_json::Value, state: AppState) -> StatusCode {
        let (status, _bytes) = send_post_json_full(uri, body, state).await;
        status
    }

    async fn send_post_json_full(
        uri: &str,
        body: serde_json::Value,
        state: AppState,
    ) -> (StatusCode, bytes::Bytes) {
        use http_body_util::BodyExt;
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes)
    }

    /// Seed a conversation, a run linked to it (with a pending feedback request),
    /// and a second unrelated conversation+run. Returns
    /// `(conv1_id, run1_id, feedback1_id, conv2_id, run2_id, feedback2_id)`.
    fn seed_conversations(
        conn: &rusqlite::Connection,
    ) -> (String, String, String, String, String, String) {
        let mgr = conductor_core::conversation::ConversationManager::new(conn);
        let agent_mgr = conductor_core::agent::AgentManager::new(conn);

        let conv1 = mgr.create(ConversationScope::Repo, "r1").unwrap();
        let run1 = agent_mgr
            .create_repo_run_for_conversation("r1", "q1", None, None, &conv1.id)
            .unwrap();
        let fb1 = agent_mgr
            .request_feedback(&run1.id, "approve?", None)
            .unwrap();

        let conv2 = mgr.create(ConversationScope::Repo, "r1").unwrap();
        let run2 = agent_mgr
            .create_repo_run_for_conversation("r1", "q2", None, None, &conv2.id)
            .unwrap();
        let fb2 = agent_mgr
            .request_feedback(&run2.id, "approve?", None)
            .unwrap();

        (conv1.id, run1.id, fb1.id, conv2.id, run2.id, fb2.id)
    }

    // ── respond_to_feedback tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn respond_to_feedback_returns_404_for_unknown_run() {
        let (state, _tmp) = seeded_state();
        {
            let db = state.db.lock().await;
            seed_conversations(&db);
        }
        let body = serde_json::json!({
            "run_id": "nonexistent-run",
            "feedback_id": "nonexistent-fb",
            "response": "yes"
        });
        let status = send_post_json("/api/conversations/any-conv/feedback", body, state).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn respond_to_feedback_returns_404_when_run_belongs_to_other_conversation() {
        let (state, _tmp) = seeded_state();
        let (conv1_id, run1_id, fb1_id, _conv2_id, _run2_id, _fb2_id) = {
            let db = state.db.lock().await;
            seed_conversations(&db)
        };
        // run1 belongs to conv1; pass conv2's ID as the path parameter
        let body = serde_json::json!({
            "run_id": run1_id,
            "feedback_id": fb1_id,
            "response": "yes"
        });
        let uri = "/api/conversations/wrong-conv-id/feedback";
        let status = send_post_json(uri, body, state).await;
        // Verify conv1_id is not used as the path parameter
        assert_ne!(conv1_id, "wrong-conv-id");
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn respond_to_feedback_returns_404_when_feedback_belongs_to_other_run() {
        let (state, _tmp) = seeded_state();
        let (conv1_id, run1_id, _fb1_id, _conv2_id, _run2_id, fb2_id) = {
            let db = state.db.lock().await;
            seed_conversations(&db)
        };
        // fb2 belongs to run2, not run1 — IDOR attempt
        let body = serde_json::json!({
            "run_id": run1_id,
            "feedback_id": fb2_id,
            "response": "yes"
        });
        let uri = format!("/api/conversations/{conv1_id}/feedback");
        let status = send_post_json(&uri, body, state).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn respond_to_feedback_returns_200_for_valid_request() {
        let (state, _tmp) = seeded_state();
        let (conv1_id, run1_id, fb1_id, _conv2_id, _run2_id, _fb2_id) = {
            let db = state.db.lock().await;
            seed_conversations(&db)
        };
        let body = serde_json::json!({
            "run_id": run1_id,
            "feedback_id": fb1_id,
            "response": "yes"
        });
        let uri = format!("/api/conversations/{conv1_id}/feedback");
        let (status, bytes) = send_post_json_full(&uri, body, state).await;
        assert_eq!(status, StatusCode::OK);
        let run: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(run["id"], run1_id);
        assert_eq!(run["conversation_id"], conv1_id);
    }

    #[tokio::test]
    async fn respond_to_run_feedback_returns_404_for_unknown_run() {
        let (state, _tmp) = seeded_state();
        {
            let db = state.db.lock().await;
            seed_conversations(&db);
        }
        let body = serde_json::json!({ "response": "yes" });
        let status = send_post_json(
            "/api/conversations/any-conv/messages/nonexistent/respond",
            body,
            state,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn respond_to_run_feedback_returns_404_when_run_belongs_to_other_conversation() {
        let (state, _tmp) = seeded_state();
        let (_conv1_id, run1_id, _fb1_id, conv2_id, _run2_id, _fb2_id) = {
            let db = state.db.lock().await;
            seed_conversations(&db)
        };
        // run1 belongs to conv1; use conv2's id in the path → ownership mismatch
        let body = serde_json::json!({ "response": "yes" });
        let uri = format!("/api/conversations/{conv2_id}/messages/{run1_id}/respond");
        let status = send_post_json(&uri, body, state).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn respond_to_run_feedback_returns_400_when_no_pending_feedback() {
        let (state, _tmp) = seeded_state();
        let (conv1_id, run1_id, _fb1_id, _conv2_id, _run2_id, _fb2_id) = {
            let db = state.db.lock().await;
            seed_conversations(&db)
        };
        let body = serde_json::json!({ "response": "yes" });
        let uri = format!("/api/conversations/{conv1_id}/messages/{run1_id}/respond");
        // First call consumes the pending feedback
        let status = send_post_json(&uri, body.clone(), state.clone()).await;
        assert_eq!(status, StatusCode::OK);
        // Second call finds no pending feedback → 400
        let status = send_post_json(&uri, body, state).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ── send_message content-type tests ──────────────────────────────────────

    /// Regression guard: JSON content-type is still accepted after the multipart refactor.
    /// The conversation doesn't exist in the DB so the response is 400 (not 415/422).
    #[tokio::test]
    async fn send_message_json_still_works() {
        let (state, _tmp) = seeded_state();
        let status = send_post_json(
            "/api/conversations/nonexistent-conv/messages",
            serde_json::json!({ "prompt": "hello" }),
            state,
        )
        .await;
        assert_ne!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_ne!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// Sending `text/plain` returns 415 Unsupported Media Type.
    #[tokio::test]
    async fn send_message_unsupported_content_type_returns_415() {
        let (state, _tmp) = seeded_state();
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations/any/messages")
                    .header("content-type", "text/plain")
                    .body(Body::from("hello"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    /// Multipart with a valid `prompt` field reaches the DB lookup phase.
    /// The conversation doesn't exist, so response is 400 (not 415/422).
    #[tokio::test]
    async fn send_message_multipart_prompt_only() {
        let (state, _tmp) = seeded_state();
        let boundary = "testboundary1234";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\nhello world\r\n--{boundary}--\r\n"
        );
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations/nonexistent/messages")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Multipart parsed successfully — failure is at DB lookup, not parsing.
        assert_ne!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_ne!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// Multipart body missing the `prompt` field returns 422.
    #[tokio::test]
    async fn send_message_multipart_missing_prompt_returns_422() {
        let (state, _tmp) = seeded_state();
        let boundary = "testboundary1234";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"other_field\"\r\n\r\nsome value\r\n--{boundary}--\r\n"
        );
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations/any/messages")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// Multipart attachment with a valid MIME type but wrong magic bytes returns 422.
    #[tokio::test]
    async fn send_message_multipart_magic_bytes_mismatch_returns_422() {
        let (state, _tmp) = seeded_state();
        let boundary = "testboundarymagic";
        // Claims image/png but sends GIF bytes — magic-byte check should reject it.
        let body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"prompt\"\r\n\
             \r\n\
             hello\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"attachment1\"; filename=\"fake.png\"\r\n\
             Content-Type: image/png\r\n\
             \r\n\
             GIF89a\r\n\
             --{boundary}--\r\n"
        );
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations/any/messages")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// Multipart attachment with an unsupported MIME type returns 422.
    #[tokio::test]
    async fn send_message_multipart_unsupported_mime_returns_422() {
        let (state, _tmp) = seeded_state();
        let boundary = "testboundary5678";
        // Build a multipart body with a valid `prompt` and a `video/mp4` attachment.
        let body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"prompt\"\r\n\
             \r\n\
             hello\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"attachment1\"; filename=\"clip.mp4\"\r\n\
             Content-Type: video/mp4\r\n\
             \r\n\
             fakevideobytes\r\n\
             --{boundary}--\r\n"
        );
        let app = api_router().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations/any/messages")
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn respond_to_run_feedback_returns_200_for_valid_request() {
        let (state, _tmp) = seeded_state();
        let (conv1_id, run1_id, _fb1_id, _conv2_id, _run2_id, _fb2_id) = {
            let db = state.db.lock().await;
            seed_conversations(&db)
        };
        let body = serde_json::json!({ "response": "yes" });
        let uri = format!("/api/conversations/{conv1_id}/messages/{run1_id}/respond");
        let (status, bytes) = send_post_json_full(&uri, body, state).await;
        assert_eq!(status, StatusCode::OK);
        let run: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(run["id"], run1_id);
        assert_eq!(run["conversation_id"], conv1_id);
    }
}
