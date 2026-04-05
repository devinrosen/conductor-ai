use axum::extract::{Path, Query, State};
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

use crate::error::ApiError;
use crate::state::AppState;

use super::agents::spawn_tmux_blocking;

// ── Request / Response types ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateConversationRequest {
    pub scope: ConversationScope,
    pub scope_id: String,
}

#[derive(Deserialize)]
pub struct ListConversationsQuery {
    pub scope: ConversationScope,
    pub scope_id: String,
}

#[derive(Deserialize)]
pub struct SendMessageRequest {
    pub prompt: String,
}

#[derive(Deserialize)]
pub struct RespondToFeedbackRequest {
    pub response: String,
}

#[derive(Deserialize)]
pub struct RespondToFeedbackByIdRequest {
    pub run_id: String,
    pub feedback_id: String,
    pub response: String,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// POST /api/conversations — create a new conversation.
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
/// Creates a new agent run, with automatic session resumption from the last
/// completed run. Returns the full `AgentRun` object immediately; the agent
/// runs asynchronously.
pub async fn send_message(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    Json(body): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<AgentRun>), ApiError> {
    // Phase 1: all DB work under the async lock.
    let (run, resume_session_id, working_dir, permission_mode, model, window_name) = {
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

        // Derive a tmux window name from the conversation ID prefix.
        let conv_prefix = &conversation_id[..8.min(conversation_id.len())];
        let window_name = format!("conv-{conv_prefix}");

        // Delegate run creation, concurrency guard, session lookup, and
        // metadata updates to ConversationManager::send_message.
        let (run, resume_session_id) = conv_mgr.send_message(
            &conversation_id,
            &body.prompt,
            Some(&window_name),
            model.as_deref(),
        )?;

        (
            run,
            resume_session_id,
            working_dir,
            permission_mode,
            model,
            window_name,
        )
    };
    // DB and config locks are now dropped.

    // Phase 2: build args and spawn the tmux window.
    let args = match permission_mode {
        Some(ref mode) => conductor_core::agent_runtime::build_agent_args_with_mode(
            &run.id,
            &working_dir,
            &body.prompt,
            resume_session_id.as_deref(),
            model.as_deref(),
            None,
            Some(mode),
            &[],
        )
        .map_err(ConductorError::Agent)?,
        None => conductor_core::agent_runtime::build_agent_args(
            &run.id,
            &working_dir,
            &body.prompt,
            resume_session_id.as_deref(),
            model.as_deref(),
            None,
            &[],
        )
        .map_err(ConductorError::Agent)?,
    };

    spawn_tmux_blocking(&state, &run.id, args, window_name).await?;

    Ok((StatusCode::CREATED, Json(run)))
}

/// POST /api/conversations/{id}/feedback — submit feedback for a run using an
/// explicit feedback_id. This is the mobile-client entrypoint; the run_id is
/// used only to verify the run belongs to the conversation.
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
}
