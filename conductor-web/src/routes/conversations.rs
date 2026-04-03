use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

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

#[derive(Serialize)]
pub struct SendMessageResponse {
    pub agent_run_id: String,
}

#[derive(Deserialize)]
pub struct RespondToFeedbackRequest {
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
        ConductorError::Agent(format!("conversation {conversation_id} not found"))
    })?;
    Ok(Json(conversation))
}

/// POST /api/conversations/{id}/message — send a message to a conversation.
///
/// Creates a new agent run, with automatic session resumption from the last
/// completed run. Returns `{ agent_run_id }` immediately; the agent runs
/// asynchronously.
pub async fn send_message(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    Json(body): Json<SendMessageRequest>,
) -> Result<(StatusCode, Json<SendMessageResponse>), ApiError> {
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
                    Some(AgentPermissionMode::Plan),
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

    Ok((
        StatusCode::CREATED,
        Json(SendMessageResponse {
            agent_run_id: run.id,
        }),
    ))
}

/// POST /api/conversations/{id}/message/{run_id}/respond — respond to a
/// human-in-the-loop feedback request for a specific run.
pub async fn respond_to_run_feedback(
    State(state): State<AppState>,
    Path((conversation_id, run_id)): Path<(String, String)>,
    Json(body): Json<RespondToFeedbackRequest>,
) -> Result<Json<AgentRun>, ApiError> {
    let db = state.db.lock().await;
    let agent_mgr = AgentManager::new(&db);

    // Validate the run belongs to this conversation.
    let run = agent_mgr
        .get_run(&run_id)?
        .ok_or_else(|| ConductorError::Agent(format!("agent run {run_id} not found")))?;
    if run.conversation_id.as_deref() != Some(&conversation_id) {
        return Err(ConductorError::Agent(
            "agent run does not belong to this conversation".to_string(),
        )
        .into());
    }

    // Find the pending feedback request for this run.
    let feedback = agent_mgr
        .pending_feedback_for_run(&run_id)?
        .ok_or_else(|| {
            ConductorError::Agent(format!("no pending feedback request for run {run_id}"))
        })?;

    agent_mgr.submit_feedback(&feedback.id, &body.response)?;

    // Return the refreshed run record.
    let updated_run = agent_mgr
        .get_run(&run_id)?
        .ok_or_else(|| ConductorError::Agent(format!("agent run {run_id} not found")))?;

    Ok(Json(updated_run))
}
