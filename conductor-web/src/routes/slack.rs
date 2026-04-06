use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use conductor_core::notify::format_active_runs_for_slack;
use conductor_core::workflow::WorkflowManager;

use crate::state::AppState;

/// Slack sends slash command payloads as application/x-www-form-urlencoded.
#[derive(Debug, Deserialize)]
pub struct SlackSlashCommand {
    pub command: Option<String>,
    pub text: Option<String>,
    #[allow(dead_code)]
    pub response_url: Option<String>,
    #[allow(dead_code)]
    pub user_name: Option<String>,
}

/// Slack expects a JSON response with a `text` field (and optional `response_type`).
#[derive(Serialize)]
struct SlackResponse {
    response_type: &'static str,
    text: String,
}

/// Verify the `X-Slack-Signature` header using HMAC-SHA256.
///
/// Returns `Ok(())` if the signature is valid or no signing secret is configured
/// (to allow gradual rollout). Returns `Err` with a status code if verification fails.
fn verify_slack_signature(
    signing_secret: Option<&str>,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), StatusCode> {
    let secret = match signing_secret {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(()), // no secret configured — skip verification
    };

    let timestamp = headers
        .get("X-Slack-Request-Timestamp")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let sig_header = headers
        .get("X-Slack-Signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let base_string = format!("v0:{timestamp}:{}", String::from_utf8_lossy(body));
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| StatusCode::UNAUTHORIZED)?;
    mac.update(base_string.as_bytes());
    let result = mac.finalize();
    let expected = format!("v0={}", hex::encode(result.into_bytes()));

    if expected != sig_header {
        tracing::warn!("Slack signature verification failed");
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(())
}

/// Handle Slack slash commands.
///
/// Supports:
///   /conductor active   — list active workflow runs
///   /conductor help     — show available commands
pub async fn handle_slash_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Verify Slack request signature when a signing secret is configured.
    {
        let config = state.config.read().await;
        let secret = config.notifications.slack.signing_secret.as_deref();
        if let Err(status) = verify_slack_signature(secret, &headers, &body) {
            return (
                status,
                axum::Json(SlackResponse {
                    response_type: "ephemeral",
                    text: "Request signature verification failed.".to_string(),
                }),
            );
        }
    }

    let payload: SlackSlashCommand = match serde_urlencoded::from_bytes(&body) {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(SlackResponse {
                    response_type: "ephemeral",
                    text: "Invalid request payload.".to_string(),
                }),
            );
        }
    };

    let subcommand = payload.text.as_deref().unwrap_or("").trim();

    let response = match subcommand.split_whitespace().next().unwrap_or("help") {
        "active" => handle_active(&state).await,
        _ => SlackResponse {
            response_type: "ephemeral",
            text: concat!(
                "*Available commands:*\n",
                "• `/conductor active` — list active workflow runs\n",
                "• `/conductor help` — show this message",
            )
            .to_string(),
        },
    };

    (StatusCode::OK, axum::Json(response))
}

async fn handle_active(state: &AppState) -> SlackResponse {
    let db = state.db.lock().await;
    let wf_mgr = WorkflowManager::new(&db);

    let runs = match wf_mgr.list_active_workflow_runs(&[]) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "Failed to list active workflow runs for Slack command");
            return SlackResponse {
                response_type: "ephemeral",
                text: format!("Error querying workflows: {e}"),
            };
        }
    };

    SlackResponse {
        response_type: "in_channel",
        text: format_active_runs_for_slack(&runs),
    }
}
